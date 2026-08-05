//! Multi-profile retrieval selection and degradation policy.
//!
//! Two embedding profiles that were trained separately produce distances on
//! incomparable scales, so this module never compares raw scores across
//! profiles. It decides *which* profiles serve a query and *how much rank
//! weight* each contributes; the executor then fuses their ranks through the
//! existing weighted reciprocal-rank prefetch tree.
//!
//! The failure mode this exists to prevent is a silently partial answer. A
//! request either declares that every profile must serve, in which case a
//! missing one fails closed, or it accepts partial coverage — and then the
//! decision records exactly which profiles were missing.

use std::collections::BTreeSet;

use context_core::{ProfileId, ProfileLifecycle, SearchLimit};

use crate::{QueryError, Result};

/// Maximum profiles that may serve one multi-model query.
pub const MAX_MULTI_PROFILE_BRANCHES: usize = 8;

/// One declared profile branch and its rank-fusion weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileBranch {
    profile: ProfileId,
    weight: f64,
    limit: SearchLimit,
}

impl ProfileBranch {
    /// Creates a validated profile branch.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the weight is not finite and
    /// strictly positive. A zero weight is rejected rather than silently
    /// dropping the branch: a caller that wants a profile excluded should not
    /// declare it.
    pub fn new(profile: ProfileId, weight: f64, limit: SearchLimit) -> Result<Self> {
        if !weight.is_finite() || weight <= 0.0 {
            return Err(QueryError::InvalidInput {
                field: "profile_weight",
                reason: "must be finite and strictly positive".to_owned(),
            });
        }
        Ok(Self {
            profile,
            weight,
            limit,
        })
    }

    /// Returns the profile identity.
    #[must_use]
    pub const fn profile(self) -> ProfileId {
        self.profile
    }

    /// Returns the rank-fusion weight.
    #[must_use]
    pub const fn weight(self) -> f64 {
        self.weight
    }

    /// Returns the per-profile candidate limit.
    #[must_use]
    pub const fn limit(self) -> SearchLimit {
        self.limit
    }
}

/// Validated multi-profile retrieval request.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiProfileRequest {
    branches: Vec<ProfileBranch>,
    require_all_profiles: bool,
    rank_constant: u32,
}

impl MultiProfileRequest {
    /// Creates a validated multi-profile request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the branch set is empty,
    /// exceeds [`MAX_MULTI_PROFILE_BRANCHES`], repeats a profile, or when the
    /// rank constant is zero.
    pub fn new(
        branches: Vec<ProfileBranch>,
        require_all_profiles: bool,
        rank_constant: u32,
    ) -> Result<Self> {
        if branches.is_empty() || branches.len() > MAX_MULTI_PROFILE_BRANCHES {
            return Err(QueryError::InvalidInput {
                field: "profile_branches",
                reason: format!("must declare 1..={MAX_MULTI_PROFILE_BRANCHES} profiles"),
            });
        }
        let unique = branches
            .iter()
            .map(|branch| branch.profile())
            .collect::<BTreeSet<_>>();
        if unique.len() != branches.len() {
            return Err(QueryError::InvalidInput {
                field: "profile_branches",
                reason: "must not repeat a profile".to_owned(),
            });
        }
        if rank_constant == 0 {
            return Err(QueryError::InvalidInput {
                field: "rank_constant",
                reason: "must be positive".to_owned(),
            });
        }
        Ok(Self {
            branches,
            require_all_profiles,
            rank_constant,
        })
    }

    /// Returns the declared branches.
    #[must_use]
    pub fn branches(&self) -> &[ProfileBranch] {
        &self.branches
    }

    /// Reports whether every declared profile must serve.
    #[must_use]
    pub const fn require_all_profiles(&self) -> bool {
        self.require_all_profiles
    }

    /// Returns the reciprocal-rank fusion constant.
    #[must_use]
    pub const fn rank_constant(&self) -> u32 {
        self.rank_constant
    }
}

/// Why a declared profile could not serve.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProfileUnavailability {
    /// The profile is not registered on the collection.
    Unregistered,
    /// The profile is registered but its lifecycle state does not serve.
    NotServing {
        /// Observed lifecycle state.
        lifecycle: ProfileLifecycle,
    },
    /// The query vector does not match the profile's declared contract.
    ContractMismatch,
}

impl ProfileUnavailability {
    /// Returns the stable diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Unregistered => "unregistered",
            Self::NotServing { .. } => "not_serving",
            Self::ContractMismatch => "contract_mismatch",
        }
    }
}

/// One declared profile that could not serve, and why.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MissingProfile {
    profile: ProfileId,
    reason: ProfileUnavailability,
}

impl MissingProfile {
    /// Records a declared profile that could not serve.
    #[must_use]
    pub const fn new(profile: ProfileId, reason: ProfileUnavailability) -> Self {
        Self { profile, reason }
    }

    /// Returns the profile identity.
    #[must_use]
    pub const fn profile(self) -> ProfileId {
        self.profile
    }

    /// Returns why it could not serve.
    #[must_use]
    pub const fn reason(self) -> ProfileUnavailability {
        self.reason
    }
}

/// Coverage achieved by a multi-profile decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MultiProfileCoverage {
    /// Every declared profile serves.
    Complete,
    /// Some declared profiles could not serve, and the caller allowed it.
    Partial {
        /// Declared profiles that could not serve, in profile order.
        missing: Vec<MissingProfile>,
    },
}

/// Outcome of resolving a multi-profile request against observed availability.
#[derive(Clone, Debug, PartialEq)]
pub enum MultiProfileDecision {
    /// Serve these branches, with this coverage.
    Serve {
        /// Branches that will actually execute.
        branches: Vec<ProfileBranch>,
        /// Whether coverage is complete or explicitly partial.
        coverage: MultiProfileCoverage,
    },
    /// Refuse to serve rather than return a partial answer.
    FailClosed {
        /// Declared profiles that could not serve, in profile order.
        missing: Vec<MissingProfile>,
    },
}

/// Resolves a declared request against observed profile availability.
///
/// `available` reports, for each declared profile, either that it serves or why
/// it does not. A profile the caller declared but `available` never mentions is
/// treated as unregistered rather than quietly skipped.
///
/// There is deliberately no third outcome: either every declared profile serves
/// (`Complete`), the caller accepted partial coverage and gets the missing set
/// named (`Partial`), or the request fails closed. A partial answer is never
/// returned as if it were complete.
///
/// # Errors
///
/// Returns [`QueryError::InvalidInput`] when every declared profile is missing
/// and partial coverage was allowed, because an empty branch set cannot be
/// fused into a result.
pub fn plan_multi_profile(
    request: &MultiProfileRequest,
    available: impl Fn(ProfileId) -> Option<ProfileUnavailability>,
) -> Result<MultiProfileDecision> {
    let mut serving = Vec::with_capacity(request.branches().len());
    let mut missing = Vec::new();
    for branch in request.branches() {
        match available(branch.profile()) {
            None => serving.push(*branch),
            Some(reason) => missing.push(MissingProfile::new(branch.profile(), reason)),
        }
    }
    missing.sort();

    if missing.is_empty() {
        return Ok(MultiProfileDecision::Serve {
            branches: serving,
            coverage: MultiProfileCoverage::Complete,
        });
    }
    if request.require_all_profiles() {
        return Ok(MultiProfileDecision::FailClosed { missing });
    }
    if serving.is_empty() {
        return Err(QueryError::InvalidInput {
            field: "profile_branches",
            reason: "no declared profile can serve this query".to_owned(),
        });
    }
    Ok(MultiProfileDecision::Serve {
        branches: serving,
        coverage: MultiProfileCoverage::Partial { missing },
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn profile(id: u64) -> ProfileId {
        ProfileId::new(id).expect("non-zero profile")
    }

    fn limit() -> SearchLimit {
        SearchLimit::new(10).expect("limit")
    }

    fn branch(id: u64, weight: f64) -> ProfileBranch {
        ProfileBranch::new(profile(id), weight, limit()).expect("branch")
    }

    fn request(branches: Vec<ProfileBranch>, require_all: bool) -> MultiProfileRequest {
        MultiProfileRequest::new(branches, require_all, 60).expect("request")
    }

    #[test]
    fn branch_weights_must_be_finite_and_strictly_positive() {
        for weight in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                ProfileBranch::new(profile(1), weight, limit()).is_err(),
                "weight {weight} must be rejected"
            );
        }
        assert!(ProfileBranch::new(profile(1), f64::MIN_POSITIVE, limit()).is_ok());
    }

    #[test]
    fn requests_reject_empty_oversized_and_duplicated_branch_sets() {
        assert!(MultiProfileRequest::new(Vec::new(), false, 60).is_err());
        let oversized = (1..=MAX_MULTI_PROFILE_BRANCHES + 1)
            .map(|id| branch(id as u64, 1.0))
            .collect::<Vec<_>>();
        assert!(MultiProfileRequest::new(oversized, false, 60).is_err());
        assert!(MultiProfileRequest::new(vec![branch(1, 1.0), branch(1, 2.0)], false, 60).is_err());
        assert!(MultiProfileRequest::new(vec![branch(1, 1.0)], false, 0).is_err());
    }

    #[test]
    fn every_profile_serving_reports_complete_coverage() {
        let request = request(vec![branch(1, 1.0), branch(2, 0.5)], true);
        let decision = plan_multi_profile(&request, |_| None).expect("decision");
        let MultiProfileDecision::Serve { branches, coverage } = decision else {
            unreachable!("a fully available request must serve");
        };
        assert_eq!(branches.len(), 2);
        assert_eq!(coverage, MultiProfileCoverage::Complete);
    }

    #[test]
    fn require_all_profiles_fails_closed_on_a_missing_profile() {
        let request = request(vec![branch(1, 1.0), branch(2, 1.0)], true);
        let decision = plan_multi_profile(&request, |candidate| {
            (candidate == profile(2)).then_some(ProfileUnavailability::Unregistered)
        })
        .expect("decision");
        assert_eq!(
            decision,
            MultiProfileDecision::FailClosed {
                missing: vec![MissingProfile::new(
                    profile(2),
                    ProfileUnavailability::Unregistered
                )]
            }
        );
    }

    #[test]
    fn partial_coverage_names_every_missing_profile_and_its_reason() {
        let request = request(vec![branch(1, 1.0), branch(2, 1.0), branch(3, 1.0)], false);
        let decision = plan_multi_profile(&request, |candidate| {
            if candidate == profile(2) {
                Some(ProfileUnavailability::NotServing {
                    lifecycle: ProfileLifecycle::Shadow,
                })
            } else if candidate == profile(3) {
                Some(ProfileUnavailability::ContractMismatch)
            } else {
                None
            }
        })
        .expect("decision");
        let MultiProfileDecision::Serve { branches, coverage } = decision else {
            unreachable!("partial coverage must still serve");
        };
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].profile(), profile(1));
        let MultiProfileCoverage::Partial { missing } = coverage else {
            unreachable!("coverage must be reported as partial");
        };
        assert_eq!(
            missing,
            vec![
                MissingProfile::new(
                    profile(2),
                    ProfileUnavailability::NotServing {
                        lifecycle: ProfileLifecycle::Shadow
                    }
                ),
                MissingProfile::new(profile(3), ProfileUnavailability::ContractMismatch),
            ]
        );
    }

    #[test]
    fn a_request_whose_every_profile_is_missing_is_an_error_not_an_empty_answer() {
        let request = request(vec![branch(1, 1.0), branch(2, 1.0)], false);
        assert!(matches!(
            plan_multi_profile(&request, |_| Some(ProfileUnavailability::Unregistered)),
            Err(QueryError::InvalidInput {
                field: "profile_branches",
                ..
            })
        ));
    }

    #[test]
    fn a_shadow_profile_is_reported_as_not_serving_rather_than_unregistered() {
        let request = request(vec![branch(1, 1.0), branch(2, 1.0)], true);
        let decision = plan_multi_profile(&request, |candidate| {
            (candidate == profile(2)).then_some(ProfileUnavailability::NotServing {
                lifecycle: ProfileLifecycle::Shadow,
            })
        })
        .expect("decision");
        let MultiProfileDecision::FailClosed { missing } = decision else {
            unreachable!("require_all must fail closed");
        };
        assert_eq!(missing[0].reason().stable_name(), "not_serving");
    }
}
