/// Semantic mutation steps emitted before physical page application.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GraphMutationStep {
    /// Append a bounded node payload as unpublished.
    AppendUnpublishedNode(GraphAppendStep),
    /// Store one complete outbound adjacency layer for the new node.
    WriteOutboundLayer(GraphAdjacencyStep),
    /// Replace one existing complete adjacency layer with optimistic revision.
    ReplaceNeighbors(GraphAdjacencyStep),
    /// Mark a fully connected unpublished node ready.
    MarkNodeReady(GraphReadyStep),
    /// Publish reader-visible metadata last.
    Publish(GraphPublicationStep),
}

/// Observable insertion progress for repair decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInsertPhase {
    /// No page state has been applied.
    Prepared,
    /// Unpublished node payload exists.
    NodeAppended,
    /// Outbound adjacency layers are being written.
    WritingOutbound {
        /// Number of complete outbound layers already stored.
        completed: usize,
        /// Required outbound layer count.
        total: usize,
    },
    /// Existing neighbor layers are being replaced.
    Rewiring {
        /// Number of complete existing layers already replaced.
        completed: usize,
        /// Required rewire count.
        total: usize,
    },
    /// Outbound layers and rewires are complete; node may be marked ready.
    ReadyToMarkNode,
    /// Node is ready but replacement metadata is not published.
    ReadyToPublish,
    /// Replacement metadata is reader-visible.
    Published,
}

impl GraphInsertPhase {
    /// Returns the stable version-two persisted phase code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Prepared => 0,
            Self::NodeAppended => 1,
            Self::WritingOutbound { .. } => 2,
            Self::Rewiring { .. } => 3,
            Self::ReadyToMarkNode => 4,
            Self::ReadyToPublish => 5,
            Self::Published => 6,
        }
    }
}

/// Event applied to one insertion plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphInsertEvent {
    mutation_id: GraphMutationId,
    node_id: HnswNodeId,
    kind: GraphInsertEventKind,
}

impl GraphInsertEvent {
    /// Creates an insertion event with explicit ownership.
    #[must_use]
    pub const fn new(
        mutation_id: GraphMutationId,
        node_id: HnswNodeId,
        kind: GraphInsertEventKind,
    ) -> Self {
        Self {
            mutation_id,
            node_id,
            kind,
        }
    }
}

/// Idempotently replayable insertion event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphInsertEventKind {
    /// Unpublished node payload was appended.
    NodeAppended,
    /// One zero-based outbound layer ordinal was stored.
    OutboundLayerWritten {
        /// Zero-based layer ordinal.
        ordinal: usize,
    },
    /// One zero-based rewire ordinal was applied.
    RewireApplied {
        /// Zero-based deterministic rewire ordinal.
        ordinal: usize,
    },
    /// Node publication state became ready.
    NodeReady,
    /// Replacement reader metadata was published.
    Published {
        /// Complete reader state observed while holding the publication lock.
        observed_state: GraphPublishedState,
    },
}

impl GraphInsertEventKind {
    const fn name(self) -> &'static str {
        match self {
            Self::NodeAppended => "node_appended",
            Self::OutboundLayerWritten { .. } => "outbound_layer_written",
            Self::RewireApplied { .. } => "rewire_applied",
            Self::NodeReady => "node_ready",
            Self::Published { .. } => "published",
        }
    }
}

/// Per-insertion state; multiple plans may coexist and interleave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphInsertPlan {
    reservation: GraphNodeReservation,
    record_id: GraphRecordId,
    dimensions: usize,
    outbound_total: usize,
    rewires_total: usize,
    entry_point_after: Option<HnswNodeId>,
    previous: GraphPublishedState,
    node_appended: bool,
    outbound_completed: usize,
    rewires_completed: usize,
    node_ready: bool,
    published: Option<GraphPublishedState>,
}

impl GraphInsertPlan {
    /// Creates a bounded insertion plan without changing reader state.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] for zero dimensions,
    /// excessive layers/rewires, dimension drift, or a missing first root.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        reservation: GraphNodeReservation,
        record_id: GraphRecordId,
        dimensions: usize,
        outbound_total: usize,
        rewires_total: usize,
        entry_point_after: Option<HnswNodeId>,
        previous: GraphPublishedState,
    ) -> GraphMutationResult<Self> {
        let node_id = reservation.node_id();
        let max_rewires = MAX_GRAPH_LAYERS.saturating_mul(MAX_GRAPH_NEIGHBORS_PER_LAYER);
        if dimensions == 0
            || outbound_total == 0
            || outbound_total > MAX_GRAPH_LAYERS
            || rewires_total > max_rewires
            || previous
                .dimensions()
                .is_some_and(|value| value != dimensions)
            || (previous.node_count() == 0 && entry_point_after.is_none())
            || (entry_point_after != Some(node_id) && entry_point_after != previous.entry_point())
        {
            return Err(GraphMutationError::InvalidPlan {
                reason: "dimensions, layer/rewire bounds, or target root are invalid",
            });
        }
        Ok(Self {
            reservation,
            record_id,
            dimensions,
            outbound_total,
            rewires_total,
            entry_point_after,
            previous,
            node_appended: false,
            outbound_completed: 0,
            rewires_completed: 0,
            node_ready: false,
            published: None,
        })
    }

    /// Returns the mutation owner.
    #[must_use]
    pub const fn mutation_id(&self) -> GraphMutationId {
        self.reservation.mutation_id()
    }

    /// Returns the reserved node id.
    #[must_use]
    pub const fn node_id(&self) -> HnswNodeId {
        self.reservation.node_id()
    }

    /// Returns the adapter-owned record token.
    #[must_use]
    pub const fn record_id(&self) -> GraphRecordId {
        self.record_id
    }

    /// Prepares the complete persisted descriptor header for this phase.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::ArithmeticOverflow`] if the target
    /// published generation or node count cannot be represented.
    pub fn descriptor(
        &self,
        descriptor_revision: GraphRecordRevision,
    ) -> GraphMutationResult<GraphMutationDescriptor> {
        Ok(GraphMutationDescriptor {
            reservation: self.reservation,
            record_id: self.record_id,
            descriptor_revision,
            phase: self.phase(),
            outbound_total: self.outbound_total,
            outbound_completed: self.outbound_completed,
            rewires_total: self.rewires_total,
            rewires_completed: self.rewires_completed,
            expected_state: self.previous,
            target_state: self.target_published()?,
        })
    }

    /// Prepares the unpublished-node append step bound to this reservation.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] unless the plan is prepared
    /// and vector/layer dimensions match the validated plan.
    pub fn append_step(
        &self,
        vector: DenseVector,
        layer_count: GraphLayerCount,
    ) -> GraphMutationResult<GraphMutationStep> {
        if self.phase() != GraphInsertPhase::Prepared
            || vector.dimension() != self.dimensions
            || layer_count.get() != self.outbound_total
        {
            return Err(GraphMutationError::InvalidPlan {
                reason: "append payload does not match the prepared insertion",
            });
        }
        Ok(GraphMutationStep::AppendUnpublishedNode(GraphAppendStep {
            reservation: self.reservation,
            record_id: self.record_id,
            vector,
            layer_count,
            target_generation: self.target_published()?.generation(),
        }))
    }

    /// Prepares one complete outbound adjacency step for the new node.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::NodeMismatch`] for another node or
    /// [`GraphMutationError::InvalidPlan`] for a layer outside the plan.
    pub fn outbound_step(
        &self,
        adjacency: GraphNeighbors,
    ) -> GraphMutationResult<GraphMutationStep> {
        if adjacency.node_id() != self.node_id() {
            return Err(GraphMutationError::NodeMismatch {
                expected: self.node_id(),
                actual: adjacency.node_id(),
            });
        }
        if adjacency.layer().get() >= self.outbound_total {
            return Err(GraphMutationError::InvalidPlan {
                reason: "outbound adjacency layer exceeds the prepared node",
            });
        }
        Ok(GraphMutationStep::WriteOutboundLayer(GraphAdjacencyStep {
            mutation_id: self.mutation_id(),
            inserted_node_id: self.node_id(),
            adjacency,
            expected_revision: None,
            target_generation: self.target_published()?.generation(),
        }))
    }

    /// Prepares one complete existing-node rewire with optimistic revision.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] if the replacement targets
    /// the new node rather than an existing node.
    pub fn rewire_step(
        &self,
        adjacency: GraphNeighbors,
        expected_revision: GraphRecordRevision,
    ) -> GraphMutationResult<GraphMutationStep> {
        if adjacency.node_id() == self.node_id() || !adjacency.neighbors().contains(&self.node_id())
        {
            return Err(GraphMutationError::InvalidPlan {
                reason: "rewire step must target an existing node and link the inserted node",
            });
        }
        Ok(GraphMutationStep::ReplaceNeighbors(GraphAdjacencyStep {
            mutation_id: self.mutation_id(),
            inserted_node_id: self.node_id(),
            adjacency,
            expected_revision: Some(expected_revision),
            target_generation: self.target_published()?.generation(),
        }))
    }

    /// Prepares the ready-state step after all topology writes.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidTransition`] unless the plan is
    /// ready to mark its node.
    pub fn ready_step(
        &self,
        expected_revision: GraphRecordRevision,
    ) -> GraphMutationResult<GraphMutationStep> {
        if self.phase() != GraphInsertPhase::ReadyToMarkNode {
            return Err(GraphMutationError::InvalidTransition {
                phase: self.phase(),
                event: "prepare_node_ready",
            });
        }
        Ok(GraphMutationStep::MarkNodeReady(GraphReadyStep {
            reservation: self.reservation,
            expected_revision,
            target_generation: self.target_published()?.generation(),
        }))
    }

    /// Prepares final reader-state publication from this validated plan.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidTransition`] unless the node is
    /// ready, or arithmetic/state errors while deriving replacement metadata.
    pub fn publication_step(&self) -> GraphMutationResult<GraphMutationStep> {
        if self.phase() != GraphInsertPhase::ReadyToPublish {
            return Err(GraphMutationError::InvalidTransition {
                phase: self.phase(),
                event: "prepare_publication",
            });
        }
        Ok(GraphMutationStep::Publish(GraphPublicationStep {
            reservation: self.reservation,
            expected_state: self.previous,
            new_state: self.target_published()?,
        }))
    }

    /// Returns the current resumable phase.
    #[must_use]
    pub const fn phase(&self) -> GraphInsertPhase {
        if self.published.is_some() {
            GraphInsertPhase::Published
        } else if self.node_ready {
            GraphInsertPhase::ReadyToPublish
        } else if self.outbound_completed == self.outbound_total
            && self.rewires_completed == self.rewires_total
        {
            GraphInsertPhase::ReadyToMarkNode
        } else if self.rewires_completed > 0 || self.outbound_completed == self.outbound_total {
            GraphInsertPhase::Rewiring {
                completed: self.rewires_completed,
                total: self.rewires_total,
            }
        } else if self.outbound_completed > 0 {
            GraphInsertPhase::WritingOutbound {
                completed: self.outbound_completed,
                total: self.outbound_total,
            }
        } else if self.node_appended {
            GraphInsertPhase::NodeAppended
        } else {
            GraphInsertPhase::Prepared
        }
    }

    /// Returns the only state readers may observe.
    #[must_use]
    pub fn visible_state(&self) -> GraphPublishedState {
        self.published.unwrap_or(self.previous)
    }

    /// Classifies an interruption at the current prefix.
    #[must_use]
    pub fn interruption_availability(&self) -> GraphAvailability {
        let reason = match self.phase() {
            GraphInsertPhase::Prepared | GraphInsertPhase::Published => {
                return GraphAvailability::Ready;
            }
            GraphInsertPhase::NodeAppended => GraphRepairReason::InterruptedAppend,
            GraphInsertPhase::WritingOutbound { .. } => GraphRepairReason::InterruptedOutbound,
            GraphInsertPhase::Rewiring { .. } => GraphRepairReason::InterruptedRewire,
            GraphInsertPhase::ReadyToMarkNode => GraphRepairReason::InterruptedNodePublication,
            GraphInsertPhase::ReadyToPublish => GraphRepairReason::InterruptedPublication,
        };
        GraphAvailability::RepairRequired {
            mutation_id: self.mutation_id(),
            reason,
        }
    }

    /// Applies one owned event functionally, leaving `self` unchanged on error.
    ///
    /// Replaying an already applied event is a no-op. Events may not skip an
    /// ordinal or cross mutation/node ownership.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError`] for ownership conflicts, skipped phases,
    /// or publication arithmetic overflow.
    pub fn transition(mut self, event: GraphInsertEvent) -> GraphMutationResult<Self> {
        if event.mutation_id != self.mutation_id() {
            return Err(GraphMutationError::MutationMismatch {
                expected: self.mutation_id(),
                actual: event.mutation_id,
            });
        }
        if event.node_id != self.node_id() {
            return Err(GraphMutationError::NodeMismatch {
                expected: self.node_id(),
                actual: event.node_id,
            });
        }
        let before = self.phase();
        match event.kind {
            GraphInsertEventKind::NodeAppended => {
                self.node_appended = true;
            }
            GraphInsertEventKind::OutboundLayerWritten { ordinal } => {
                if !self.node_appended || ordinal > self.outbound_completed {
                    return Err(invalid_transition(before, event.kind));
                }
                if ordinal == self.outbound_completed {
                    if ordinal >= self.outbound_total {
                        return Err(invalid_transition(before, event.kind));
                    }
                    self.outbound_completed += 1;
                }
            }
            GraphInsertEventKind::RewireApplied { ordinal } => {
                if !self.node_appended
                    || self.outbound_completed != self.outbound_total
                    || ordinal > self.rewires_completed
                {
                    return Err(invalid_transition(before, event.kind));
                }
                if ordinal == self.rewires_completed {
                    if ordinal >= self.rewires_total {
                        return Err(invalid_transition(before, event.kind));
                    }
                    self.rewires_completed += 1;
                }
            }
            GraphInsertEventKind::NodeReady => {
                if !self.node_appended
                    || self.outbound_completed != self.outbound_total
                    || self.rewires_completed != self.rewires_total
                {
                    return Err(invalid_transition(before, event.kind));
                }
                self.node_ready = true;
            }
            GraphInsertEventKind::Published { observed_state } => {
                if !self.node_ready {
                    return Err(invalid_transition(before, event.kind));
                }
                let target = self.target_published()?;
                if let Some(published) = self.published {
                    if observed_state == published {
                        return Ok(self);
                    }
                    return Err(publication_conflict(published, observed_state));
                }
                if observed_state == target {
                    self.published = Some(target);
                    return Ok(self);
                }
                if observed_state != self.previous {
                    return Err(publication_conflict(self.previous, observed_state));
                }
                self.published = Some(target);
            }
        }
        Ok(self)
    }

    fn target_published(&self) -> GraphMutationResult<GraphPublishedState> {
        let generation = self.previous.generation().checked_add(1).ok_or(
            GraphMutationError::ArithmeticOverflow {
                operation: "publish graph generation",
            },
        )?;
        let node_count = self.previous.node_count().checked_add(1).ok_or(
            GraphMutationError::ArithmeticOverflow {
                operation: "publish graph node count",
            },
        )?;
        GraphPublishedState::new_with_tombstones(
            generation,
            node_count,
            self.previous.tombstone_count(),
            self.entry_point_after,
            Some(self.dimensions),
            self.previous.format_version(),
            Some(self.mutation_id()),
        )
    }
}

fn invalid_transition(phase: GraphInsertPhase, event: GraphInsertEventKind) -> GraphMutationError {
    GraphMutationError::InvalidTransition {
        phase,
        event: event.name(),
    }
}

fn publication_conflict(
    expected: GraphPublishedState,
    actual: GraphPublishedState,
) -> GraphMutationError {
    GraphMutationError::PublicationConflict {
        expected_generation: expected.generation(),
        expected_mutation_id: expected.last_mutation_id(),
        actual_generation: actual.generation(),
        actual_mutation_id: actual.last_mutation_id(),
    }
}
