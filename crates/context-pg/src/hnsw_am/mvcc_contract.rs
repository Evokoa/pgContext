//! Pure PostgreSQL HNSW MVCC, tombstone, and source-recheck contract.
//!
//! This module owns no buffers, snapshots, callbacks, or heap tuples. It
//! validates bounded decisions that the physical access-method adapter must
//! make before entering WAL critical sections.

#![cfg_attr(test, allow(clippy::expect_used, clippy::panic))]

use core::{fmt, num::NonZeroU64};

use context_core::PointId;
use context_index::{GraphRecordId, GraphRecordRevision, GraphTombstoneStep, HnswNodeId};

pub(super) const MAX_HNSW_VACUUM_CALLBACK_BATCH: usize = 64;

pub(super) type HnswMvccResult<T> = Result<T, HnswMvccError>;

/// PostgreSQL physical heap tuple address.
///
/// This intentionally does not implement conversions to logical point, graph
/// node, or graph record identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct HnswHeapTid {
    block: u32,
    offset: u16,
}

impl HnswHeapTid {
    pub(super) const fn new(block: u32, offset: u16) -> HnswMvccResult<Self> {
        if block == u32::MAX || offset == 0 {
            return Err(HnswMvccError::InvalidHeapTid);
        }
        Ok(Self { block, offset })
    }

    pub(super) const fn block(self) -> u32 {
        self.block
    }

    pub(super) const fn offset(self) -> u16 {
        self.offset
    }
}

/// Exact source identity persisted with one graph node record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswSourceBinding {
    node_id: HnswNodeId,
    record_id: GraphRecordId,
    heap_tid: HnswHeapTid,
    indexed_generation: u64,
    record_revision: GraphRecordRevision,
}

impl HnswSourceBinding {
    pub(super) const fn new(
        node_id: HnswNodeId,
        record_id: GraphRecordId,
        heap_tid: HnswHeapTid,
        indexed_generation: u64,
        record_revision: GraphRecordRevision,
    ) -> HnswMvccResult<Self> {
        if indexed_generation == 0 {
            return Err(HnswMvccError::InvalidIndexedGeneration);
        }
        Ok(Self {
            node_id,
            record_id,
            heap_tid,
            indexed_generation,
            record_revision,
        })
    }

    pub(super) const fn node_id(self) -> HnswNodeId {
        self.node_id
    }

    pub(super) const fn record_id(self) -> GraphRecordId {
        self.record_id
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.heap_tid
    }

    pub(super) const fn indexed_generation(self) -> u64 {
        self.indexed_generation
    }

    pub(super) const fn record_revision(self) -> GraphRecordRevision {
        self.record_revision
    }

    const fn with_revision(self, record_revision: GraphRecordRevision) -> Self {
        Self {
            record_revision,
            ..self
        }
    }
}

/// Nonzero monotone identity assigned to a durable node tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct HnswTombstoneEpoch(NonZeroU64);

impl HnswTombstoneEpoch {
    pub(super) const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub(super) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Reader interpretation of one versioned node record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswNodeMvccState {
    /// Incomplete insertion; neither traversable nor result-eligible.
    Unpublished(HnswSourceBinding),
    /// Published topology connector that requires authoritative recheck before
    /// it can become a result.
    Ready(HnswSourceBinding),
    /// VACUUM-confirmed dead source binding; retained only as a connector.
    Tombstoned {
        binding: HnswSourceBinding,
        tombstone_epoch: HnswTombstoneEpoch,
    },
}

impl HnswNodeMvccState {
    pub(super) const fn binding(self) -> HnswSourceBinding {
        match self {
            Self::Unpublished(binding)
            | Self::Ready(binding)
            | Self::Tombstoned { binding, .. } => binding,
        }
    }

    pub(super) const fn tombstone_epoch(self) -> Option<HnswTombstoneEpoch> {
        match self {
            Self::Tombstoned {
                tombstone_epoch, ..
            } => Some(tombstone_epoch),
            Self::Unpublished(_) | Self::Ready(_) => None,
        }
    }
}

/// A dead TID established only by PostgreSQL's bulk-delete callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswVacuumDeadTid(HnswHeapTid);

impl HnswVacuumDeadTid {
    pub(super) const fn from_callback(heap_tid: HnswHeapTid) -> Self {
        Self(heap_tid)
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.0
    }
}

/// Result of invoking PostgreSQL's deletion callback with no graph lock held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswVacuumCallbackDecision {
    Keep(HnswHeapTid),
    Dead(HnswVacuumDeadTid),
}

impl HnswVacuumCallbackDecision {
    pub(super) const fn from_callback(heap_tid: HnswHeapTid, is_dead: bool) -> Self {
        if is_dead {
            Self::Dead(HnswVacuumDeadTid::from_callback(heap_tid))
        } else {
            Self::Keep(heap_tid)
        }
    }
}

/// Bounded action prepared after the unlocked callback phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswVacuumAction {
    Keep,
    AlreadyTombstoned,
    Tombstone(HnswTombstoneTransition),
}

impl HnswVacuumAction {
    pub(super) fn plan(
        decision: HnswVacuumCallbackDecision,
        observed: HnswNodeMvccState,
        tombstone_epoch: HnswTombstoneEpoch,
    ) -> HnswMvccResult<Self> {
        match decision {
            HnswVacuumCallbackDecision::Keep(heap_tid) => {
                if observed.binding().heap_tid() != heap_tid {
                    return Err(HnswMvccError::VacuumTidMismatch {
                        callback: heap_tid,
                        binding: observed.binding().heap_tid(),
                    });
                }
                Ok(Self::Keep)
            }
            HnswVacuumCallbackDecision::Dead(dead) => match observed {
                HnswNodeMvccState::Tombstoned { binding, .. } => {
                    if dead.heap_tid() != binding.heap_tid() {
                        return Err(HnswMvccError::VacuumTidMismatch {
                            callback: dead.heap_tid(),
                            binding: binding.heap_tid(),
                        });
                    }
                    Ok(Self::AlreadyTombstoned)
                }
                HnswNodeMvccState::Ready(_) => Ok(Self::Tombstone(HnswTombstoneTransition::plan(
                    dead,
                    observed,
                    tombstone_epoch,
                )?)),
                HnswNodeMvccState::Unpublished(_) => Err(HnswMvccError::NodeNotReadyForTombstone),
            },
        }
    }
}

/// Exact compare-and-replace from a ready node revision to its tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswTombstoneTransition {
    expected: HnswNodeMvccState,
    target: HnswNodeMvccState,
}

impl HnswTombstoneTransition {
    pub(super) fn plan(
        dead: HnswVacuumDeadTid,
        observed: HnswNodeMvccState,
        tombstone_epoch: HnswTombstoneEpoch,
    ) -> HnswMvccResult<Self> {
        let HnswNodeMvccState::Ready(binding) = observed else {
            return Err(HnswMvccError::NodeNotReadyForTombstone);
        };
        if dead.heap_tid() != binding.heap_tid() {
            return Err(HnswMvccError::VacuumTidMismatch {
                callback: dead.heap_tid(),
                binding: binding.heap_tid(),
            });
        }
        let target_revision = binding.record_revision().get().checked_add(1).ok_or(
            HnswMvccError::RevisionOverflow {
                node_id: binding.node_id(),
            },
        )?;
        Ok(Self {
            expected: observed,
            target: HnswNodeMvccState::Tombstoned {
                binding: binding.with_revision(GraphRecordRevision::new(target_revision)),
                tombstone_epoch,
            },
        })
    }

    pub(super) const fn expected(self) -> HnswNodeMvccState {
        self.expected
    }

    pub(super) const fn target(self) -> HnswNodeMvccState {
        self.target
    }

    pub(super) fn classify(self, observed: HnswNodeMvccState) -> HnswTombstoneApply {
        if observed == self.expected {
            HnswTombstoneApply::Apply
        } else if observed == self.target {
            HnswTombstoneApply::AlreadyApplied
        } else {
            HnswTombstoneApply::Conflict
        }
    }

    pub(super) fn bind_graph_step(
        self,
        step: GraphTombstoneStep,
    ) -> HnswMvccResult<HnswBoundTombstone> {
        let expected = self.expected.binding();
        let target = self.target.binding();
        if step.node_id() != expected.node_id()
            || step.record_id() != expected.record_id()
            || step.expected_revision() != expected.record_revision()
            || step.target_revision() != target.record_revision()
            || step.target_generation() <= expected.indexed_generation()
        {
            return Err(HnswMvccError::GraphTombstoneMismatch);
        }
        let Some(tombstone_epoch) = self.target.tombstone_epoch() else {
            return Err(HnswMvccError::GraphTombstoneMismatch);
        };
        Ok(HnswBoundTombstone {
            step,
            heap_tid: expected.heap_tid(),
            tombstone_epoch,
        })
    }
}

/// One graph-generation tombstone bound to its exact PostgreSQL source
/// identity and VACUUM epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HnswBoundTombstone {
    step: GraphTombstoneStep,
    heap_tid: HnswHeapTid,
    tombstone_epoch: HnswTombstoneEpoch,
}

impl HnswBoundTombstone {
    pub(super) const fn step(self) -> GraphTombstoneStep {
        self.step
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.heap_tid
    }

    pub(super) const fn tombstone_epoch(self) -> HnswTombstoneEpoch {
        self.tombstone_epoch
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswTombstoneApply {
    Apply,
    AlreadyApplied,
    Conflict,
}

/// Finite score recomputed from an authoritative visible source row.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub(super) struct HnswExactScore(f64);

impl HnswExactScore {
    pub(super) fn new(value: f64) -> HnswMvccResult<Self> {
        if !value.is_finite() {
            return Err(HnswMvccError::NonFiniteExactScore);
        }
        Ok(Self(value))
    }

    pub(super) const fn get(self) -> f64 {
        self.0
    }
}

/// Snapshot-visible heap row after exact ORDER BY recomputation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HnswVisibleHeapRow {
    heap_tid: HnswHeapTid,
    exact_score: HnswExactScore,
}

impl HnswVisibleHeapRow {
    pub(super) const fn new(heap_tid: HnswHeapTid, exact_score: HnswExactScore) -> Self {
        Self {
            heap_tid,
            exact_score,
        }
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.heap_tid
    }

    pub(super) const fn exact_score(self) -> HnswExactScore {
        self.exact_score
    }
}

/// Authoritative collection mapping plus exact source-row score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HnswVisiblePointRow {
    heap_tid: HnswHeapTid,
    point_id: PointId,
    exact_score: HnswExactScore,
}

impl HnswVisiblePointRow {
    pub(super) const fn new(
        heap_tid: HnswHeapTid,
        point_id: PointId,
        exact_score: HnswExactScore,
    ) -> Self {
        Self {
            heap_tid,
            point_id,
            exact_score,
        }
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.heap_tid
    }

    pub(super) const fn point_id(self) -> PointId {
        self.point_id
    }

    pub(super) const fn exact_score(self) -> HnswExactScore {
        self.exact_score
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum HnswCandidateDisposition<T> {
    Ignore,
    ConnectorOnly,
    Eligible(T),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HnswOrderedCandidate {
    heap_tid: HnswHeapTid,
    exact_score: HnswExactScore,
}

impl HnswOrderedCandidate {
    pub(super) const fn new(heap_tid: HnswHeapTid, exact_score: HnswExactScore) -> Self {
        Self {
            heap_tid,
            exact_score,
        }
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.heap_tid
    }

    pub(super) const fn exact_score(self) -> HnswExactScore {
        self.exact_score
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HnswLogicalCandidate {
    heap_tid: HnswHeapTid,
    point_id: PointId,
    exact_score: HnswExactScore,
}

impl HnswLogicalCandidate {
    pub(super) const fn new(
        heap_tid: HnswHeapTid,
        point_id: PointId,
        exact_score: HnswExactScore,
    ) -> Self {
        Self {
            heap_tid,
            point_id,
            exact_score,
        }
    }

    pub(super) const fn heap_tid(self) -> HnswHeapTid {
        self.heap_tid
    }

    pub(super) const fn point_id(self) -> PointId {
        self.point_id
    }

    pub(super) const fn exact_score(self) -> HnswExactScore {
        self.exact_score
    }
}

pub(super) fn recheck_ordered_candidate(
    state: HnswNodeMvccState,
    visible: Option<HnswVisibleHeapRow>,
) -> HnswMvccResult<HnswCandidateDisposition<HnswOrderedCandidate>> {
    match state {
        HnswNodeMvccState::Unpublished(_) => Ok(HnswCandidateDisposition::Ignore),
        HnswNodeMvccState::Tombstoned { .. } => Ok(HnswCandidateDisposition::ConnectorOnly),
        HnswNodeMvccState::Ready(binding) => {
            let Some(visible) = visible else {
                return Ok(HnswCandidateDisposition::ConnectorOnly);
            };
            validate_source_tid(binding.heap_tid(), visible.heap_tid())?;
            Ok(HnswCandidateDisposition::Eligible(
                HnswOrderedCandidate::new(visible.heap_tid(), visible.exact_score()),
            ))
        }
    }
}

pub(super) fn recheck_logical_candidate(
    state: HnswNodeMvccState,
    visible: Option<HnswVisiblePointRow>,
) -> HnswMvccResult<HnswCandidateDisposition<HnswLogicalCandidate>> {
    match state {
        HnswNodeMvccState::Unpublished(_) => Ok(HnswCandidateDisposition::Ignore),
        HnswNodeMvccState::Tombstoned { .. } => Ok(HnswCandidateDisposition::ConnectorOnly),
        HnswNodeMvccState::Ready(binding) => {
            let Some(visible) = visible else {
                return Ok(HnswCandidateDisposition::ConnectorOnly);
            };
            validate_source_tid(binding.heap_tid(), visible.heap_tid())?;
            Ok(HnswCandidateDisposition::Eligible(
                HnswLogicalCandidate::new(
                    visible.heap_tid(),
                    visible.point_id(),
                    visible.exact_score(),
                ),
            ))
        }
    }
}

fn validate_source_tid(expected: HnswHeapTid, actual: HnswHeapTid) -> HnswMvccResult<()> {
    if expected != actual {
        return Err(HnswMvccError::SourceTidMismatch { expected, actual });
    }
    Ok(())
}

pub(super) fn validate_tid_reuse(
    heap_tid: HnswHeapTid,
    existing: &[HnswNodeMvccState],
) -> HnswMvccResult<()> {
    if let Some(state) = existing.iter().copied().find(|state| {
        state.binding().heap_tid() == heap_tid
            && !matches!(state, HnswNodeMvccState::Tombstoned { .. })
    }) {
        return Err(HnswMvccError::TidReuseUnsafe {
            heap_tid,
            node_id: state.binding().node_id(),
        });
    }
    Ok(())
}

/// Allocation-free collection of callback-confirmed dead TIDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HnswVacuumBatch {
    tids: [Option<HnswVacuumDeadTid>; MAX_HNSW_VACUUM_CALLBACK_BATCH],
    len: usize,
}

impl HnswVacuumBatch {
    pub(super) const fn new() -> Self {
        Self {
            tids: [None; MAX_HNSW_VACUUM_CALLBACK_BATCH],
            len: 0,
        }
    }

    pub(super) fn record(&mut self, dead: HnswVacuumDeadTid) -> HnswMvccResult<bool> {
        if self.tids[..self.len].contains(&Some(dead)) {
            return Ok(false);
        }
        if self.len == MAX_HNSW_VACUUM_CALLBACK_BATCH {
            return Err(HnswMvccError::VacuumBatchFull {
                maximum: MAX_HNSW_VACUUM_CALLBACK_BATCH,
            });
        }
        self.tids[self.len] = Some(dead);
        self.len += 1;
        Ok(true)
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) const fn is_full(&self) -> bool {
        self.len == MAX_HNSW_VACUUM_CALLBACK_BATCH
    }

    pub(super) const fn finish_callback_phase(self) -> HnswVacuumApplyBatch {
        HnswVacuumApplyBatch {
            tids: self.tids,
            len: self.len,
        }
    }
}

impl Default for HnswVacuumBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Callback-complete inputs that may be consumed by the later locked/WAL
/// apply phase. The collection type exposes no apply iterator before this
/// explicit phase transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HnswVacuumApplyBatch {
    tids: [Option<HnswVacuumDeadTid>; MAX_HNSW_VACUUM_CALLBACK_BATCH],
    len: usize,
}

impl HnswVacuumApplyBatch {
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = HnswVacuumDeadTid> + '_ {
        self.tids[..self.len].iter().flatten().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswMvccError {
    InvalidHeapTid,
    InvalidIndexedGeneration,
    NonFiniteExactScore,
    NodeNotReadyForTombstone,
    GraphTombstoneMismatch,
    RevisionOverflow {
        node_id: HnswNodeId,
    },
    VacuumTidMismatch {
        callback: HnswHeapTid,
        binding: HnswHeapTid,
    },
    SourceTidMismatch {
        expected: HnswHeapTid,
        actual: HnswHeapTid,
    },
    TidReuseUnsafe {
        heap_tid: HnswHeapTid,
        node_id: HnswNodeId,
    },
    VacuumBatchFull {
        maximum: usize,
    },
}

impl fmt::Display for HnswMvccError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeapTid => formatter.write_str(
                "heap TID block must not be InvalidBlockNumber and offset must be nonzero",
            ),
            Self::InvalidIndexedGeneration => {
                formatter.write_str("indexed generation must be nonzero")
            }
            Self::NonFiniteExactScore => formatter.write_str("exact source score must be finite"),
            Self::NodeNotReadyForTombstone => {
                formatter.write_str("only a ready node can become a tombstone")
            }
            Self::GraphTombstoneMismatch => formatter.write_str(
                "graph tombstone step does not match physical source identity or revision",
            ),
            Self::RevisionOverflow { node_id } => {
                write!(formatter, "node revision overflow for {node_id:?}")
            }
            Self::VacuumTidMismatch { .. } => {
                formatter.write_str("VACUUM callback TID does not match the node binding")
            }
            Self::SourceTidMismatch { .. } => {
                formatter.write_str("source recheck TID does not match the node binding")
            }
            Self::TidReuseUnsafe { .. } => {
                formatter.write_str("heap TID still has a non-tombstoned graph binding")
            }
            Self::VacuumBatchFull { maximum } => {
                write!(formatter, "VACUUM callback batch exceeds {maximum} TIDs")
            }
        }
    }
}

impl std::error::Error for HnswMvccError {}

#[cfg(test)]
include!("mvcc_contract/tests.rs");
