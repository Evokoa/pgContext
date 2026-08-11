//! Bounded mixed-profile selection and rank-only fusion.
//!
//! Profiles keep their native score spaces. This module retains raw branch
//! scores only as diagnostics and combines branches exclusively by one-based
//! rank through weighted reciprocal-rank fusion.

use std::collections::{BTreeMap, BTreeSet};

use context_core::{
    PointId, ProfileLifecycle, ProfileName,
    policy::{MAX_RECALL_CHECK_POINT_IDS, MAX_SEARCH_LIMIT},
};
use context_hybrid::{RankedPoint, RrfK, WeightedRankedBranch, weighted_reciprocal_rank_fusion};

use crate::{DEFAULT_QUERY_MEMORY_BYTES, MAX_QUERY_NODES, QueryError, Result};

/// Maximum byte length of one provider-native query value.
pub const MAX_MULTI_PROFILE_QUERY_BYTES: usize = 512 * 1024;
/// Maximum profile branches that fit in the canonical root + weighted-leaf IR.
pub const MAX_MULTI_PROFILE_BRANCHES: usize = (MAX_QUERY_NODES - 1) / 2;
/// Maximum retained rows per branch, reserving one bounded completeness probe.
pub const MAX_MULTI_PROFILE_BRANCH_LIMIT: usize = MAX_SEARCH_LIMIT - 1;
/// Maximum aggregate opaque-query bytes accepted by the canonical builder.
pub const MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES: usize = DEFAULT_QUERY_MEMORY_BYTES;

/// Bounded provider-native query payload.
///
/// The query layer deliberately treats this value as opaque text. Profile
/// adapters own its interpretation and must bind it as data rather than
/// interpolating it into executable SQL.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct MultiProfileQuery(String);

impl<'de> serde::Deserialize<'de> for MultiProfileQuery {
    fn deserialize<Deserializer>(
        deserializer: Deserializer,
    ) -> core::result::Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let query = String::deserialize(deserializer)?;
        Self::new(query).map_err(serde::de::Error::custom)
    }
}

impl MultiProfileQuery {
    /// Validates and stores one provider-native query payload.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the payload is blank,
    /// contains control characters, or exceeds
    /// [`MAX_MULTI_PROFILE_QUERY_BYTES`].
    pub fn new(query: impl Into<String>) -> Result<Self> {
        let query = query.into();
        Self::validate_text(&query)?;
        Ok(Self(query))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        Self::validate_text(&self.0)
    }

    fn validate_text(query: &str) -> Result<()> {
        if query.trim().is_empty() || query.len() > MAX_MULTI_PROFILE_QUERY_BYTES {
            return Err(invalid(
                "query",
                format!("must be 1..={MAX_MULTI_PROFILE_QUERY_BYTES} bytes and nonblank"),
            ));
        }
        if query.chars().any(char::is_control) {
            return Err(invalid("query", "must not contain control characters"));
        }
        Ok(())
    }

    /// Returns the opaque provider-native query payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the value and returns the stored payload.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// One declared mixed-profile query branch.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiProfileBranch {
    profile: ProfileName,
    configuration_hash: u64,
    query: MultiProfileQuery,
    limit: usize,
    weight: f64,
}

impl MultiProfileBranch {
    /// Creates and validates one branch.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a zero configuration hash, an
    /// empty or oversized query, an invalid limit, or a non-finite/non-positive
    /// weight.
    pub fn new(
        profile: ProfileName,
        configuration_hash: u64,
        query: String,
        limit: usize,
        weight: f64,
    ) -> Result<Self> {
        if configuration_hash == 0 {
            return Err(invalid("configuration_hash", "must be nonzero"));
        }
        let query = MultiProfileQuery::new(query)?;
        if !(1..=MAX_MULTI_PROFILE_BRANCH_LIMIT).contains(&limit) {
            return Err(invalid(
                "limit",
                format!("must be between 1 and {MAX_MULTI_PROFILE_BRANCH_LIMIT}"),
            ));
        }
        if !weight.is_finite() || weight <= 0.0 {
            return Err(invalid("weight", "must be finite and greater than zero"));
        }
        Ok(Self {
            profile,
            configuration_hash,
            query,
            limit,
            weight,
        })
    }

    /// Returns the declared profile.
    #[must_use]
    pub const fn profile(&self) -> &ProfileName {
        &self.profile
    }

    /// Returns the expected immutable configuration hash.
    #[must_use]
    pub const fn configuration_hash(&self) -> u64 {
        self.configuration_hash
    }

    /// Returns the bounded provider-native query text.
    #[must_use]
    pub fn query(&self) -> &str {
        self.query.as_str()
    }

    /// Returns the bounded opaque query value.
    #[must_use]
    pub const fn typed_query(&self) -> &MultiProfileQuery {
        &self.query
    }

    pub(crate) fn into_query_parts(self) -> (ProfileName, u64, MultiProfileQuery, usize, f64) {
        (
            self.profile,
            self.configuration_hash,
            self.query,
            self.limit,
            self.weight,
        )
    }

    /// Returns the per-profile candidate limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the positive branch weight.
    #[must_use]
    pub const fn weight(&self) -> f64 {
        self.weight
    }
}

/// Validated mixed-profile query controls.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiProfileRequest {
    branches: Vec<MultiProfileBranch>,
    rrf_k: RrfK,
    limit: usize,
    unique_candidate_budget: usize,
    require_all_profiles: bool,
}

impl MultiProfileRequest {
    /// Creates a bounded request after validating all cross-branch invariants.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an empty or duplicate branch
    /// set and invalid controls. Returns [`QueryError::WorkBudgetExceeded`]
    /// when declared branch work exceeds the global candidate budget.
    pub fn new(
        branches: Vec<MultiProfileBranch>,
        rrf_k: u32,
        limit: usize,
        unique_candidate_budget: usize,
        require_all_profiles: bool,
    ) -> Result<Self> {
        if branches.is_empty() || branches.len() > MAX_MULTI_PROFILE_BRANCHES {
            return Err(invalid(
                "branches",
                format!("must contain between 1 and {MAX_MULTI_PROFILE_BRANCHES} branches"),
            ));
        }
        let aggregate_query_bytes = branches.iter().try_fold(0_usize, |total, branch| {
            total
                .checked_add(branch.typed_query().as_str().len())
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "multi_profile_query_byte_projection",
                })
        })?;
        if aggregate_query_bytes > MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "multi_profile_query_bytes",
                actual: aggregate_query_bytes,
                maximum: MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES,
            });
        }
        let mut names = BTreeSet::new();
        for branch in &branches {
            if !names.insert(branch.profile()) {
                return Err(invalid("branches", "profile names must be unique"));
            }
        }
        let Some(rrf_k) = RrfK::new(rrf_k) else {
            return Err(invalid("rrf_k", "must be greater than zero"));
        };
        if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
            return Err(invalid(
                "limit",
                format!("must be between 1 and {MAX_SEARCH_LIMIT}"),
            ));
        }
        if !(1..=MAX_RECALL_CHECK_POINT_IDS).contains(&unique_candidate_budget) {
            return Err(invalid(
                "unique_candidate_budget",
                format!("must be between 1 and {MAX_RECALL_CHECK_POINT_IDS}"),
            ));
        }
        if limit > unique_candidate_budget {
            return Err(invalid("limit", "must not exceed unique_candidate_budget"));
        }
        let declared_candidates = branches.iter().try_fold(0_usize, |total, branch| {
            let probe_limit =
                branch
                    .limit()
                    .checked_add(1)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "multi_profile_probe_projection",
                    })?;
            total
                .checked_add(probe_limit)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "multi_profile_candidate_projection",
                })
        })?;
        if declared_candidates > unique_candidate_budget {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "multi_profile_candidates",
                actual: declared_candidates,
                maximum: unique_candidate_budget,
            });
        }
        Ok(Self {
            branches,
            rrf_k,
            limit,
            unique_candidate_budget,
            require_all_profiles,
        })
    }

    /// Returns declared branches in caller order.
    #[must_use]
    pub fn branches(&self) -> &[MultiProfileBranch] {
        &self.branches
    }

    /// Returns the positive reciprocal-rank constant.
    #[must_use]
    pub const fn rrf_k(&self) -> u32 {
        self.rrf_k.get()
    }

    /// Returns the final fused result limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the global branch-work budget.
    #[must_use]
    pub const fn unique_candidate_budget(&self) -> usize {
        self.unique_candidate_budget
    }

    /// Returns whether every declared profile must be ready.
    #[must_use]
    pub const fn require_all_profiles(&self) -> bool {
        self.require_all_profiles
    }
}

/// One catalog-observed profile readiness record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiProfileObserved {
    profile: ProfileName,
    configuration_hash: u64,
    lifecycle: ProfileLifecycle,
    version_bindings_present: bool,
    ready: bool,
}

impl MultiProfileObserved {
    /// Creates a catalog observation. Adapter-level OID and permission checks
    /// remain outside this pure value.
    #[must_use]
    pub const fn new(
        profile: ProfileName,
        configuration_hash: u64,
        lifecycle: ProfileLifecycle,
        version_bindings_present: bool,
        ready: bool,
    ) -> Self {
        Self {
            profile,
            configuration_hash,
            lifecycle,
            version_bindings_present,
            ready,
        }
    }

    /// Returns the observed profile name.
    #[must_use]
    pub const fn profile(&self) -> &ProfileName {
        &self.profile
    }
}

/// Bounded reason a declared profile cannot participate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingProfileReason {
    /// No visible immutable profile registration matched the name.
    NotRegistered,
    /// The caller's immutable configuration hash does not match the catalog.
    ConfigurationChanged,
    /// The profile lifecycle does not currently serve queries.
    LifecycleNotServing,
    /// Required source/embedding version bindings are absent.
    VersionBindingsMissing,
    /// Adapter catalog/index validation reported the profile not ready.
    NotReady,
}

/// One declared profile omitted from serving with an explicit reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingProfile {
    profile: ProfileName,
    reason: MissingProfileReason,
}

impl MissingProfile {
    /// Returns the unavailable profile.
    #[must_use]
    pub const fn profile(&self) -> &ProfileName {
        &self.profile
    }

    /// Returns why it could not serve.
    #[must_use]
    pub const fn reason(&self) -> MissingProfileReason {
        self.reason
    }
}

/// Coverage classification for a serving decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MultiProfileCoverage {
    /// Every declared profile is ready.
    Complete,
    /// The caller explicitly allowed the listed profiles to be skipped.
    Partial {
        /// Missing profiles in declaration order.
        missing: Vec<MissingProfile>,
    },
}

/// Pure pre-execution profile-selection decision.
#[derive(Clone, Debug, PartialEq)]
pub enum MultiProfileDecision {
    /// At least one branch may execute with explicit coverage.
    Serve {
        /// Ready branches in declaration order.
        branches: Vec<MultiProfileBranch>,
        /// Complete or explicitly partial coverage.
        coverage: MultiProfileCoverage,
    },
    /// No candidate work may begin.
    FailClosed {
        /// Unavailable declared profiles.
        missing: Vec<MissingProfile>,
    },
}

/// Resolves a validated request against catalog-observed readiness.
///
/// # Errors
///
/// Returns [`QueryError::InvalidInput`] when the adapter supplies duplicate
/// observations for one profile.
pub fn plan_multi_profile(
    request: &MultiProfileRequest,
    observed: &[MultiProfileObserved],
) -> Result<MultiProfileDecision> {
    if observed.len() > MAX_MULTI_PROFILE_BRANCHES {
        return Err(invalid(
            "observed_profiles",
            format!("must contain at most {MAX_MULTI_PROFILE_BRANCHES} profiles"),
        ));
    }
    let mut by_name = BTreeMap::new();
    for profile in observed {
        if by_name.insert(profile.profile(), profile).is_some() {
            return Err(invalid(
                "observed_profiles",
                "profile observations must be unique",
            ));
        }
    }

    let mut ready = Vec::with_capacity(request.branches().len());
    let mut missing = Vec::new();
    for branch in request.branches() {
        let reason = match by_name.get(branch.profile()).copied() {
            None => Some(MissingProfileReason::NotRegistered),
            Some(profile) if profile.configuration_hash != branch.configuration_hash() => {
                Some(MissingProfileReason::ConfigurationChanged)
            }
            Some(profile) if !profile.lifecycle.serves_queries() => {
                Some(MissingProfileReason::LifecycleNotServing)
            }
            Some(profile) if !profile.version_bindings_present => {
                Some(MissingProfileReason::VersionBindingsMissing)
            }
            Some(profile) if !profile.ready => Some(MissingProfileReason::NotReady),
            Some(_) => None,
        };
        if let Some(reason) = reason {
            missing.push(MissingProfile {
                profile: branch.profile().clone(),
                reason,
            });
        } else {
            ready.push(branch.clone());
        }
    }

    if ready.is_empty() || (request.require_all_profiles() && !missing.is_empty()) {
        return Ok(MultiProfileDecision::FailClosed { missing });
    }
    let coverage = if missing.is_empty() {
        MultiProfileCoverage::Complete
    } else {
        MultiProfileCoverage::Partial { missing }
    };
    Ok(MultiProfileDecision::Serve {
        branches: ready,
        coverage,
    })
}

/// One finite profile-native score at an authoritative branch rank.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MultiProfileRankedCandidate {
    point_id: PointId,
    raw_score: f64,
}

impl MultiProfileRankedCandidate {
    /// Creates a ranked candidate.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a non-finite raw score.
    pub fn new(point_id: PointId, raw_score: f64) -> Result<Self> {
        if !raw_score.is_finite() {
            return Err(invalid("raw_score", "must be finite"));
        }
        Ok(Self {
            point_id,
            raw_score,
        })
    }
}

/// One already-ranked profile branch supplied to fusion.
#[derive(Clone, Copy, Debug)]
pub struct MultiProfileRankedBranch<'a> {
    profile: &'a ProfileName,
    weight: f64,
    candidates: &'a [MultiProfileRankedCandidate],
}

impl<'a> MultiProfileRankedBranch<'a> {
    /// Creates a ranked branch with a finite positive weight.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an invalid weight.
    pub fn new(
        profile: &'a ProfileName,
        weight: f64,
        candidates: &'a [MultiProfileRankedCandidate],
    ) -> Result<Self> {
        if !weight.is_finite() || weight <= 0.0 {
            return Err(invalid("weight", "must be finite and greater than zero"));
        }
        Ok(Self {
            profile,
            weight,
            candidates,
        })
    }
}

/// One profile's retained contribution to a fused result.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiProfileContribution {
    profile: ProfileName,
    rank: u32,
    raw_score: f64,
    weight: f64,
    contribution: f64,
}

impl MultiProfileContribution {
    /// Returns the contributing profile.
    #[must_use]
    pub const fn profile(&self) -> &ProfileName {
        &self.profile
    }

    /// Returns the one-based branch rank.
    #[must_use]
    pub const fn rank(&self) -> u32 {
        self.rank
    }

    /// Returns the native raw score retained only for diagnostics.
    #[must_use]
    pub const fn raw_score(&self) -> f64 {
        self.raw_score
    }

    /// Returns the declared, unnormalized branch weight.
    #[must_use]
    pub const fn weight(&self) -> f64 {
        self.weight
    }

    /// Returns this branch's normalized reciprocal-rank contribution.
    #[must_use]
    pub const fn contribution(&self) -> f64 {
        self.contribution
    }
}

/// One stable-ID deduplicated fused result.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiProfileFusedPoint {
    point_id: PointId,
    score: f64,
    contributions: Vec<MultiProfileContribution>,
}

impl MultiProfileFusedPoint {
    /// Returns the stable logical point identifier.
    #[must_use]
    pub const fn point_id(&self) -> PointId {
        self.point_id
    }

    /// Returns the weighted reciprocal-rank score.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }

    /// Returns retained contributions in branch declaration order.
    #[must_use]
    pub fn contributions(&self) -> &[MultiProfileContribution] {
        &self.contributions
    }
}

/// Fuses authoritative per-profile ranks without comparing native scores.
///
/// # Errors
///
/// Returns [`QueryError::InvalidInput`] for empty branches or invalid controls,
/// and [`QueryError::WorkBudgetExceeded`] when branch rows exceed the global
/// candidate budget.
pub fn fuse_multi_profile(
    branches: &[MultiProfileRankedBranch<'_>],
    rrf_k: u32,
    limit: usize,
    candidate_budget: usize,
) -> Result<Vec<MultiProfileFusedPoint>> {
    if !(1..=MAX_MULTI_PROFILE_BRANCHES).contains(&branches.len()) {
        return Err(invalid(
            "branches",
            format!("must contain between 1 and {MAX_MULTI_PROFILE_BRANCHES} branches"),
        ));
    }
    let Some(k) = RrfK::new(rrf_k) else {
        return Err(invalid("rrf_k", "must be greater than zero"));
    };
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(invalid(
            "limit",
            format!("must be between 1 and {MAX_SEARCH_LIMIT}"),
        ));
    }
    if !(1..=MAX_RECALL_CHECK_POINT_IDS).contains(&candidate_budget) || limit > candidate_budget {
        return Err(invalid(
            "candidate_budget",
            format!("must be between the result limit and {MAX_RECALL_CHECK_POINT_IDS}"),
        ));
    }
    let candidate_count = branches.iter().try_fold(0_usize, |total, branch| {
        total
            .checked_add(branch.candidates.len())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_fusion_projection",
            })
    })?;
    if candidate_count > candidate_budget {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_candidates",
            actual: candidate_count,
            maximum: candidate_budget,
        });
    }
    let total_weight = branches.iter().map(|branch| branch.weight).sum::<f64>();
    if !total_weight.is_finite() || total_weight <= 0.0 {
        return Err(invalid(
            "weight",
            "branch weights must have a finite positive sum",
        ));
    }

    let ranked_storage = branches
        .iter()
        .map(|branch| {
            branch
                .candidates
                .iter()
                .map(|candidate| RankedPoint::new(candidate.point_id.get()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let weighted = branches
        .iter()
        .zip(&ranked_storage)
        .map(|(branch, points)| WeightedRankedBranch::new(points, branch.weight))
        .collect::<Vec<_>>();
    let fused = weighted_reciprocal_rank_fusion(&weighted, k, limit).map_err(|error| {
        invalid(
            "weight",
            format!("weighted reciprocal-rank fusion failed: {error:?}"),
        )
    })?;

    let retained = fused
        .iter()
        .map(|point| point.point_id())
        .collect::<BTreeSet<_>>();
    let mut contributions = BTreeMap::<u64, Vec<MultiProfileContribution>>::new();
    for branch in branches {
        let normalized_weight = branch.weight / total_weight;
        let mut seen = BTreeSet::new();
        for (index, candidate) in branch.candidates.iter().enumerate() {
            if !seen.insert(candidate.point_id) || !retained.contains(&candidate.point_id.get()) {
                continue;
            }
            let rank = u32::try_from(index.saturating_add(1)).map_err(|_| {
                QueryError::ArithmeticOverflow {
                    operation: "multi_profile_rank_conversion",
                }
            })?;
            let contribution = normalized_weight / (f64::from(k.get()) + f64::from(rank));
            contributions
                .entry(candidate.point_id.get())
                .or_default()
                .push(MultiProfileContribution {
                    profile: branch.profile.clone(),
                    rank,
                    raw_score: candidate.raw_score,
                    weight: branch.weight,
                    contribution,
                });
        }
    }

    fused
        .into_iter()
        .map(|point| {
            let Some(contributions) = contributions.remove(&point.point_id()) else {
                return Err(QueryError::InvariantViolation {
                    operation: "multi_profile_contribution_join",
                });
            };
            Ok(MultiProfileFusedPoint {
                point_id: PointId::new(point.point_id()),
                score: point.score(),
                contributions,
            })
        })
        .collect()
}

fn invalid(field: &'static str, reason: impl Into<String>) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: reason.into(),
    }
}
