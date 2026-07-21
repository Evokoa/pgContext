//! Pure contracts for resumable pgContext generation builds.
//!
//! PostgreSQL job persistence, WAL, files, and background execution remain in
//! infrastructure adapters. This crate depends only on `context-core`.

pub use context_core::PointId;

/// The pgContext-owned generation family a job produces.
///
/// Native PostgreSQL `CREATE INDEX` work is intentionally not represented:
/// this contract coordinates only pgContext artifact and projection output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildJobKind {
    /// A derived, replaceable pgContext artifact.
    Artifact,
    /// A derived, replaceable pgContext projection.
    Projection,
}

/// Durable lifecycle status for a resumable generation job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildJobStatus {
    /// Metadata was created but work has not started.
    Planned,
    /// A runner owns and is advancing the job.
    Running,
    /// A runner must stop at its next safe checkpoint.
    CancelRequested,
    /// The runner stopped after observing cancellation.
    Cancelled,
    /// All source work and activation completed.
    Completed,
    /// The runner recorded a recoverable failure.
    Failed,
    /// The recorded runner disappeared before reaching a terminal state.
    Abandoned,
}

/// Monotonic source-work position persisted by an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCheckpoint {
    processed_units: u64,
    total_units: u64,
}

impl BuildCheckpoint {
    /// Creates a checkpoint with a bounded amount of completed work.
    ///
    /// # Errors
    ///
    /// Returns [`BuildTransitionError::ProgressExceedsTotal`] when `processed`
    /// is greater than `total`.
    pub const fn new(processed_units: u64, total_units: u64) -> Result<Self, BuildTransitionError> {
        if processed_units > total_units {
            return Err(BuildTransitionError::ProgressExceedsTotal);
        }
        Ok(Self {
            processed_units,
            total_units,
        })
    }

    /// Returns the completed source-work units.
    #[must_use]
    pub const fn processed_units(self) -> u64 {
        self.processed_units
    }

    /// Returns the total source-work units.
    #[must_use]
    pub const fn total_units(self) -> u64 {
        self.total_units
    }

    /// Returns whether all source work has been checkpointed.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.processed_units == self.total_units
    }

    /// Advances by a bounded, nonzero amount without exceeding total work.
    ///
    /// Reapplying a checkpoint with the same or a smaller completed position is
    /// deliberately idempotent, which permits at-least-once adapter retries.
    ///
    /// # Errors
    ///
    /// Returns [`BuildTransitionError::ZeroStep`] for a zero step.
    pub const fn advance(self, step: u64) -> Result<Self, BuildTransitionError> {
        if step == 0 {
            return Err(BuildTransitionError::ZeroStep);
        }
        let remaining = self.total_units - self.processed_units;
        let advance = if step > remaining { remaining } else { step };
        Ok(Self {
            processed_units: self.processed_units + advance,
            total_units: self.total_units,
        })
    }
}

/// Pure durable state needed to validate lifecycle transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildJobState {
    kind: BuildJobKind,
    status: BuildJobStatus,
    attempt: u32,
    checkpoint: BuildCheckpoint,
}

impl BuildJobState {
    /// Creates a planned first attempt.
    #[must_use]
    pub const fn planned(kind: BuildJobKind, total_units: u64) -> Self {
        Self {
            kind,
            status: BuildJobStatus::Planned,
            attempt: 1,
            checkpoint: BuildCheckpoint {
                processed_units: 0,
                total_units,
            },
        }
    }

    /// Returns the owned generation family.
    #[must_use]
    pub const fn kind(self) -> BuildJobKind {
        self.kind
    }

    /// Returns the lifecycle status.
    #[must_use]
    pub const fn status(self) -> BuildJobStatus {
        self.status
    }

    /// Returns the one-based retry attempt.
    #[must_use]
    pub const fn attempt(self) -> u32 {
        self.attempt
    }

    /// Returns the monotonic source-work checkpoint.
    #[must_use]
    pub const fn checkpoint(self) -> BuildCheckpoint {
        self.checkpoint
    }

    /// Starts planned work or resumes a retry attempt.
    ///
    /// # Errors
    ///
    /// Returns [`BuildTransitionError::InvalidTransition`] for terminal or
    /// already-running jobs.
    pub const fn start(mut self) -> Result<Self, BuildTransitionError> {
        match self.status {
            BuildJobStatus::Planned => {
                self.status = BuildJobStatus::Running;
                Ok(self)
            }
            BuildJobStatus::Failed | BuildJobStatus::Cancelled | BuildJobStatus::Abandoned => {
                self.attempt += 1;
                self.status = BuildJobStatus::Running;
                Ok(self)
            }
            _ => Err(BuildTransitionError::InvalidTransition),
        }
    }

    /// Records a cooperative cancellation request.
    ///
    /// Repeating a cancellation request is idempotent.
    pub const fn request_cancel(mut self) -> Result<Self, BuildTransitionError> {
        match self.status {
            BuildJobStatus::Running => {
                self.status = BuildJobStatus::CancelRequested;
                Ok(self)
            }
            BuildJobStatus::CancelRequested => Ok(self),
            _ => Err(BuildTransitionError::InvalidTransition),
        }
    }

    /// Advances one bounded source-work step.
    ///
    /// A completed checkpoint transitions directly to `Completed`; a pending
    /// cancellation transitions to `Cancelled` without advancing work.
    pub const fn advance(mut self, step: u64) -> Result<Self, BuildTransitionError> {
        match self.status {
            BuildJobStatus::CancelRequested => {
                self.status = BuildJobStatus::Cancelled;
                Ok(self)
            }
            BuildJobStatus::Running => {
                self.checkpoint = match self.checkpoint.advance(step) {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => return Err(error),
                };
                if self.checkpoint.is_complete() {
                    self.status = BuildJobStatus::Completed;
                }
                Ok(self)
            }
            _ => Err(BuildTransitionError::InvalidTransition),
        }
    }

    /// Records a recoverable runner failure.
    pub const fn fail(mut self) -> Result<Self, BuildTransitionError> {
        match self.status {
            BuildJobStatus::Running | BuildJobStatus::CancelRequested => {
                self.status = BuildJobStatus::Failed;
                Ok(self)
            }
            _ => Err(BuildTransitionError::InvalidTransition),
        }
    }

    /// Records loss of a nonterminal runner.
    pub const fn abandon(mut self) -> Result<Self, BuildTransitionError> {
        match self.status {
            BuildJobStatus::Running | BuildJobStatus::CancelRequested => {
                self.status = BuildJobStatus::Abandoned;
                Ok(self)
            }
            _ => Err(BuildTransitionError::InvalidTransition),
        }
    }
}

/// A rejected pure lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildTransitionError {
    /// A checkpoint exceeded its declared total.
    ProgressExceedsTotal,
    /// A runner step must process at least one unit.
    ZeroStep,
    /// The requested lifecycle transition is not legal from the current state.
    InvalidTransition,
}

/// Returns the version of the pure build boundary.
#[must_use]
pub const fn build_contract_version() -> u16 {
    2
}

#[cfg(test)]
mod tests {
    use super::{
        BuildJobKind, BuildJobState, BuildJobStatus, BuildTransitionError, PointId,
        build_contract_version,
    };
    use proptest::prelude::*;

    #[allow(clippy::panic, reason = "test helper reports an unexpected transition")]
    fn transition(result: Result<BuildJobState, BuildTransitionError>) -> BuildJobState {
        match result {
            Ok(state) => state,
            Err(error) => panic!("expected valid build transition, got {error:?}"),
        }
    }

    #[test]
    fn build_boundary_uses_logical_point_ids() {
        let point_id = PointId::new(11);
        assert_eq!(point_id.get(), 11);
        assert_eq!(build_contract_version(), 2);
    }

    #[test]
    fn retries_preserve_the_checkpoint_and_increment_attempt_once() {
        let state = transition(
            transition(
                transition(
                    transition(BuildJobState::planned(BuildJobKind::Artifact, 4).start())
                        .advance(2),
                )
                .fail(),
            )
            .start(),
        );
        assert_eq!(state.status(), BuildJobStatus::Running);
        assert_eq!(state.attempt(), 2);
        assert_eq!(state.checkpoint().processed_units(), 2);
    }

    #[test]
    fn cancellation_request_is_idempotent_and_does_not_advance_work() {
        let state = transition(
            transition(
                transition(
                    transition(BuildJobState::planned(BuildJobKind::Projection, 3).start())
                        .request_cancel(),
                )
                .request_cancel(),
            )
            .advance(1),
        );
        assert_eq!(state.status(), BuildJobStatus::Cancelled);
        assert_eq!(state.checkpoint().processed_units(), 0);
    }

    #[test]
    fn invalid_state_changes_fail_closed() {
        let state = BuildJobState::planned(BuildJobKind::Artifact, 1);
        assert_eq!(
            state.advance(1),
            Err(BuildTransitionError::InvalidTransition)
        );
        assert_eq!(
            transition(state.start()).advance(0),
            Err(BuildTransitionError::ZeroStep)
        );
    }

    proptest! {
        #[test]
        fn retries_are_idempotent_for_the_same_persisted_checkpoint(
            total in 1_u64..128,
            completed in 0_u64..128,
        ) {
            prop_assume!(completed <= total);
            let state = transition(
                transition(BuildJobState::planned(BuildJobKind::Artifact, total).start())
                    .advance(completed.max(1)),
            );
            let state = if state.status() == BuildJobStatus::Completed {
                state
            } else {
                transition(transition(state.fail()).start())
            };
            prop_assert!(state.checkpoint().processed_units() <= total);
            prop_assert!(state.attempt() <= 2);
        }
    }
}
