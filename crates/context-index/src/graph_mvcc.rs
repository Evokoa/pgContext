//! Storage-agnostic HNSW tombstone publication state machine.
//!
//! The model preserves an older published generation until a complete
//! tombstone node version has been stored and replacement metadata is
//! published. It does not perform heap visibility checks or page I/O.

use crate::{
    GraphMutationId, GraphNodePublication, GraphPublishedState, GraphRecordId, GraphRecordRevision,
    HnswNodeId,
};

/// Result type for graph MVCC planning.
pub type GraphMvccResult<T> = Result<T, GraphMvccError>;

/// How an HNSW node may participate in a reader operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeUse {
    /// Do not traverse or return the node.
    Ignore,
    /// Traverse the node and require authoritative source recheck before use as
    /// a result candidate.
    TraverseAndRecheck,
    /// Retain the node only as a topology connector.
    TraverseOnly,
}

impl GraphNodePublication {
    /// Returns the stable version-two node-state code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Unpublished => 0,
            Self::Ready => 1,
            Self::Tombstoned => 2,
        }
    }

    /// Returns the reader use permitted by this publication state.
    #[must_use]
    pub const fn node_use(self) -> GraphNodeUse {
        match self {
            Self::Unpublished => GraphNodeUse::Ignore,
            Self::Ready => GraphNodeUse::TraverseAndRecheck,
            Self::Tombstoned => GraphNodeUse::TraverseOnly,
        }
    }
}

/// Exact versioned node write that precedes tombstone publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphTombstoneStep {
    mutation_id: GraphMutationId,
    node_id: HnswNodeId,
    record_id: GraphRecordId,
    expected_revision: GraphRecordRevision,
    target_revision: GraphRecordRevision,
    target_generation: u64,
}

impl GraphTombstoneStep {
    /// Returns the owning mutation.
    #[must_use]
    pub const fn mutation_id(self) -> GraphMutationId {
        self.mutation_id
    }

    /// Returns the graph-local node identity.
    #[must_use]
    pub const fn node_id(self) -> HnswNodeId {
        self.node_id
    }

    /// Returns the exact adapter record identity being replaced.
    #[must_use]
    pub const fn record_id(self) -> GraphRecordId {
        self.record_id
    }

    /// Returns the required current node-record revision.
    #[must_use]
    pub const fn expected_revision(self) -> GraphRecordRevision {
        self.expected_revision
    }

    /// Returns the next node-record revision stored by the tombstone.
    #[must_use]
    pub const fn target_revision(self) -> GraphRecordRevision {
        self.target_revision
    }

    /// Returns the unpublished generation of the tombstone record and locator.
    #[must_use]
    pub const fn target_generation(self) -> u64 {
        self.target_generation
    }
}

/// Resumable two-step tombstone storage and metadata-publication plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphTombstonePlan {
    step: GraphTombstoneStep,
    previous: GraphPublishedState,
    target: GraphPublishedState,
    stored: bool,
    published: bool,
}

/// Opaque metadata-publication step available only after the exact tombstone
/// node version has been reported stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphTombstonePublicationStep {
    tombstone: GraphTombstoneStep,
    expected: GraphPublishedState,
    target: GraphPublishedState,
}

impl GraphTombstonePublicationStep {
    /// Returns the stored tombstone identity whose generation is published.
    #[must_use]
    pub const fn tombstone(self) -> GraphTombstoneStep {
        self.tombstone
    }

    /// Returns the complete state required before publication.
    #[must_use]
    pub const fn expected_state(self) -> GraphPublishedState {
        self.expected
    }

    /// Returns the complete state installed by publication.
    #[must_use]
    pub const fn target_state(self) -> GraphPublishedState {
        self.target
    }
}

impl GraphTombstonePlan {
    /// Creates a tombstone plan without changing reader-visible state.
    ///
    /// # Errors
    ///
    /// Rejects empty/all-tombstoned graphs, out-of-span node IDs, and
    /// generation, revision, or tombstone-count overflow.
    pub fn new(
        mutation_id: GraphMutationId,
        node_id: HnswNodeId,
        record_id: GraphRecordId,
        expected_revision: GraphRecordRevision,
        previous: GraphPublishedState,
    ) -> GraphMvccResult<Self> {
        if previous.node_count() == 0
            || previous.candidate_node_count() == 0
            || node_id.get() >= previous.node_count()
        {
            return Err(GraphMvccError::InvalidPlan {
                reason: "tombstone target is outside the candidate-bearing node span",
            });
        }
        let target_generation =
            previous
                .generation()
                .checked_add(1)
                .ok_or(GraphMvccError::ArithmeticOverflow {
                    operation: "publish tombstone generation",
                })?;
        let target_revision =
            expected_revision
                .get()
                .checked_add(1)
                .ok_or(GraphMvccError::ArithmeticOverflow {
                    operation: "advance tombstone node revision",
                })?;
        let tombstone_count = previous.tombstone_count().checked_add(1).ok_or(
            GraphMvccError::ArithmeticOverflow {
                operation: "increment tombstone count",
            },
        )?;
        let target = GraphPublishedState::new_with_tombstones(
            target_generation,
            previous.node_count(),
            tombstone_count,
            previous.entry_point(),
            previous.dimensions(),
            previous.format_version(),
            Some(mutation_id),
        )
        .map_err(|_| GraphMvccError::InvalidPlan {
            reason: "target tombstone publication state is invalid",
        })?;
        Ok(Self {
            step: GraphTombstoneStep {
                mutation_id,
                node_id,
                record_id,
                expected_revision,
                target_revision: GraphRecordRevision::new(target_revision),
                target_generation,
            },
            previous,
            target,
            stored: false,
            published: false,
        })
    }

    /// Returns the exact node-version store step.
    #[must_use]
    pub const fn store_step(&self) -> GraphTombstoneStep {
        self.step
    }

    /// Returns the complete metadata state published last.
    #[must_use]
    pub const fn target_state(&self) -> GraphPublishedState {
        self.target
    }

    /// Returns the complete published state that must still be current before
    /// metadata publication.
    #[must_use]
    pub const fn expected_state(&self) -> GraphPublishedState {
        self.previous
    }

    /// Produces the standalone metadata-publication step after the exact node
    /// version has finished storing.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMvccError::InvalidTransition`] before storage completes.
    pub fn publication_step(&self) -> GraphMvccResult<GraphTombstonePublicationStep> {
        if !self.stored {
            return Err(GraphMvccError::InvalidTransition {
                reason: "tombstone publication step requires stored node version",
            });
        }
        Ok(GraphTombstonePublicationStep {
            tombstone: self.step,
            expected: self.previous,
            target: self.target,
        })
    }

    /// Returns the only metadata state readers may observe at this prefix.
    #[must_use]
    pub const fn visible_state(&self) -> GraphPublishedState {
        if self.published {
            self.target
        } else {
            self.previous
        }
    }

    /// Records completion of the exact versioned tombstone write.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMvccError::StoreConflict`] for any different node,
    /// record, generation, or revision identity.
    pub fn record_store_finished(mut self, observed: GraphTombstoneStep) -> GraphMvccResult<Self> {
        if observed != self.step {
            return Err(GraphMvccError::StoreConflict);
        }
        self.stored = true;
        Ok(self)
    }

    /// Records completion of standalone metapage publication.
    ///
    /// # Errors
    ///
    /// Rejects publication before storage and any complete state other than the
    /// exact target for this mutation.
    pub fn record_publication_finished(
        mut self,
        observed: GraphPublishedState,
    ) -> GraphMvccResult<Self> {
        if !self.stored {
            return Err(GraphMvccError::InvalidTransition {
                reason: "tombstone metadata cannot publish before node storage",
            });
        }
        if observed != self.target {
            return Err(GraphMvccError::PublicationConflict {
                expected_generation: self.target.generation(),
                actual_generation: observed.generation(),
            });
        }
        self.published = true;
        Ok(self)
    }

    /// Returns whether storage and publication have both finished.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.stored && self.published
    }
}

/// Pure graph tombstone planning failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GraphMvccError {
    /// Input state cannot produce a legal tombstone transition.
    #[error("invalid graph tombstone plan: {reason}")]
    InvalidPlan {
        /// Stable bounded reason.
        reason: &'static str,
    },
    /// A monotone generation, revision, or count overflowed.
    #[error("graph tombstone arithmetic overflow while attempting to {operation}")]
    ArithmeticOverflow {
        /// Stable bounded operation name.
        operation: &'static str,
    },
    /// The completed node write does not match the prepared step.
    #[error("graph tombstone store identity or revision conflict")]
    StoreConflict,
    /// Publication was attempted at an invalid plan prefix.
    #[error("invalid graph tombstone transition: {reason}")]
    InvalidTransition {
        /// Stable bounded reason.
        reason: &'static str,
    },
    /// Metapage completion did not observe the exact target state.
    #[error(
        "graph tombstone publication conflict: expected generation {expected_generation}, got {actual_generation}"
    )]
    PublicationConflict {
        /// Target generation.
        expected_generation: u64,
        /// Observed conflicting generation.
        actual_generation: u64,
    },
}
