//! Embedding-profile lifecycle states and their legal transitions.
//!
//! A mixed corpus is served by more than one embedding profile at once, so a
//! profile needs a state that says whether it accepts writes, serves queries,
//! or neither. Making that a validated state machine rather than a mutable
//! boolean is the point: a profile cannot be resurrected from a terminal state,
//! and "is this serving?" has exactly one answer in exactly one place.

use crate::{Error, Result};

/// Lifecycle state of one immutable embedding profile.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProfileLifecycle {
    /// Registered and accepting writes, but not serving queries.
    ///
    /// This is where a new model backfills before it is trusted to answer.
    Shadow,
    /// Serving queries.
    Active,
    /// Still serving queries while being phased out.
    ///
    /// A cutover keeps the outgoing profile here so in-flight coverage does not
    /// disappear the instant the replacement goes active.
    Draining,
    /// No longer serving or accepting writes. Terminal.
    Retired,
    /// Withdrawn after a failure. Serves nothing until deliberately revived.
    Failed,
}

impl ProfileLifecycle {
    /// Every state, in registration-to-terminal order.
    pub const ALL: [Self; 5] = [
        Self::Shadow,
        Self::Active,
        Self::Draining,
        Self::Retired,
        Self::Failed,
    ];

    /// Returns the stable catalog and diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Retired => "retired",
            Self::Failed => "failed",
        }
    }

    /// Parses a stable lifecycle name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] for an unrecognized state name.
    pub fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|state| state.stable_name() == name)
            .ok_or_else(|| {
                Error::InvalidVector(format!("unsupported embedding profile lifecycle: {name}"))
            })
    }

    /// Reports whether a profile in this state answers queries.
    ///
    /// Only `active` and `draining` serve. `shadow` is still backfilling and
    /// would answer from partial coverage; `retired` and `failed` answer
    /// nothing.
    #[must_use]
    pub const fn serves_queries(self) -> bool {
        matches!(self, Self::Active | Self::Draining)
    }

    /// Reports whether a profile in this state accepts new embeddings.
    #[must_use]
    pub const fn accepts_writes(self) -> bool {
        matches!(self, Self::Shadow | Self::Active | Self::Draining)
    }

    /// Reports whether this state is terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Retired)
    }

    /// Reports whether `self` may transition directly to `next`.
    ///
    /// A state never transitions to itself: re-declaring the current state is a
    /// no-op the caller should recognize rather than a lifecycle event.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            // Promotion out of backfill, or abandonment before ever serving.
            (Self::Shadow, Self::Active | Self::Failed | Self::Retired) => true,
            // Begin a cutover, or withdraw a profile that started misbehaving.
            (Self::Active, Self::Draining | Self::Failed) => true,
            // Finish the cutover, roll it back, or withdraw mid-cutover.
            (Self::Draining, Self::Retired | Self::Active | Self::Failed) => true,
            // A failure can be retried from backfill or accepted as final.
            (Self::Failed, Self::Shadow | Self::Retired) => true,
            _ => false,
        }
    }

    /// Validates a transition and returns the next state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the transition is not legal,
    /// including a transition out of a terminal state and a transition to the
    /// current state.
    pub fn transition_to(self, next: Self) -> Result<Self> {
        if self.can_transition_to(next) {
            return Ok(next);
        }
        Err(Error::InvalidVector(format!(
            "embedding profile cannot move from {} to {}",
            self.stable_name(),
            next.stable_name()
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn stable_names_round_trip() {
        for state in ProfileLifecycle::ALL {
            assert_eq!(
                ProfileLifecycle::parse(state.stable_name()).expect("known state"),
                state
            );
        }
        assert!(ProfileLifecycle::parse("paused").is_err());
    }

    #[test]
    fn only_active_and_draining_serve_queries() {
        let serving = ProfileLifecycle::ALL
            .into_iter()
            .filter(|state| state.serves_queries())
            .collect::<Vec<_>>();
        assert_eq!(
            serving,
            vec![ProfileLifecycle::Active, ProfileLifecycle::Draining]
        );
    }

    #[test]
    fn shadow_accepts_writes_without_serving() {
        assert!(ProfileLifecycle::Shadow.accepts_writes());
        assert!(!ProfileLifecycle::Shadow.serves_queries());
        for state in [ProfileLifecycle::Retired, ProfileLifecycle::Failed] {
            assert!(!state.accepts_writes(), "{state:?} must not accept writes");
            assert!(!state.serves_queries(), "{state:?} must not serve");
        }
    }

    #[test]
    fn a_cutover_walks_shadow_active_draining_retired() {
        let state = ProfileLifecycle::Shadow
            .transition_to(ProfileLifecycle::Active)
            .expect("promotion")
            .transition_to(ProfileLifecycle::Draining)
            .expect("cutover")
            .transition_to(ProfileLifecycle::Retired)
            .expect("completion");
        assert_eq!(state, ProfileLifecycle::Retired);
    }

    #[test]
    fn a_cutover_can_be_rolled_back_from_draining() {
        assert_eq!(
            ProfileLifecycle::Draining
                .transition_to(ProfileLifecycle::Active)
                .expect("rollback"),
            ProfileLifecycle::Active
        );
    }

    #[test]
    fn retired_is_terminal_and_no_state_transitions_to_itself() {
        assert!(ProfileLifecycle::Retired.is_terminal());
        for state in ProfileLifecycle::ALL {
            assert!(
                state.transition_to(state).is_err(),
                "{state:?} must not transition to itself"
            );
            assert!(
                ProfileLifecycle::Retired.transition_to(state).is_err(),
                "retired must not transition to {state:?}"
            );
        }
    }

    #[test]
    fn a_failed_profile_is_revived_only_through_backfill() {
        assert_eq!(
            ProfileLifecycle::Failed
                .transition_to(ProfileLifecycle::Shadow)
                .expect("retry"),
            ProfileLifecycle::Shadow
        );
        assert!(
            ProfileLifecycle::Failed
                .transition_to(ProfileLifecycle::Active)
                .is_err(),
            "a failed profile must re-backfill before serving again"
        );
        assert!(
            ProfileLifecycle::Failed
                .transition_to(ProfileLifecycle::Draining)
                .is_err()
        );
    }

    #[test]
    fn a_shadow_profile_never_becomes_draining_directly() {
        assert!(
            ProfileLifecycle::Shadow
                .transition_to(ProfileLifecycle::Draining)
                .is_err(),
            "draining is only meaningful for a profile that served"
        );
    }
}
