//! Versioned envelopes for provider-neutral external reranking.
//!
//! An external cross-encoder is untrusted infrastructure. It receives a bounded,
//! explicitly authorized snapshot of already-visible rows and returns an
//! ordering; it never receives database authority. Everything it can influence
//! is re-validated against the request that produced it, and the authorized
//! adapter re-applies the source recheck after provider scoring.
//!
//! The envelope is versioned because the request and the response cross a
//! process boundary: a provider built against an older contract must be
//! rejected, not silently reinterpreted.

use std::collections::BTreeSet;
use std::mem::size_of;

use context_core::{OccurrenceId, PointId, ProfileName, SourceVersion};

use crate::{QueryError, Result};

/// Envelope contract version. Bump when any field's meaning changes.
pub const RERANK_ENVELOPE_VERSION: u16 = 3;
/// Maximum candidates in one rerank request.
pub const MAX_RERANK_CANDIDATES: usize = 512;
/// Maximum bytes in the original query released to a reranker.
pub const MAX_RERANK_QUERY_BYTES: usize = 64 * 1024;
/// Maximum bytes in one model identity.
pub const MAX_RERANK_MODEL_NAME_BYTES: usize = 128;
/// Maximum authorized text bytes attached to one candidate.
pub const MAX_RERANK_TEXT_BYTES: usize = 32 * 1024;
/// Maximum serialized payload bytes across one request.
pub const MAX_RERANK_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum JSON-encoded request or response bytes on the worker wire.
pub const MAX_RERANK_WIRE_BYTES: usize = 6 * 1024 * 1024;
/// Maximum allow-listed metadata entries on one candidate.
pub const MAX_RERANK_METADATA_ENTRIES: usize = 8;
/// Maximum bytes in one metadata key or value.
pub const MAX_RERANK_METADATA_BYTES: usize = 1_024;
/// Fixed byte width of an authoritative content digest.
pub const RERANK_CONTENT_DIGEST_BYTES: usize = 32;
/// Maximum fusion branches recorded for one candidate.
pub const MAX_RERANK_CONTRIBUTIONS: usize = 127;

/// The bounded original query released to a reranker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RerankQuery(String);

impl RerankQuery {
    /// Creates a nonblank bounded query.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the query is empty, blank,
    /// contains NUL, or exceeds [`MAX_RERANK_QUERY_BYTES`].
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() > MAX_RERANK_QUERY_BYTES {
            return Err(invalid("rerank_query", "exceeds 65536 bytes"));
        }
        if value.trim().is_empty() || value.contains('\0') {
            return Err(invalid("rerank_query", "must be nonblank and NUL-free"));
        }
        Ok(Self(value))
    }

    /// Returns the original query.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Immutable model identity carried independently of its numeric revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RerankModelName(String);

impl RerankModelName {
    /// Creates a bounded control-free model identity.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the identity is blank,
    /// control-bearing, or exceeds [`MAX_RERANK_MODEL_NAME_BYTES`].
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() > MAX_RERANK_MODEL_NAME_BYTES {
            return Err(invalid("rerank_model", "exceeds 128 bytes"));
        }
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(invalid("rerank_model", "must be nonblank and control-free"));
        }
        Ok(Self(value))
    }

    /// Returns the model identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Fixed digest of the authoritative text released for a candidate.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RerankContentDigest([u8; RERANK_CONTENT_DIGEST_BYTES]);

impl RerankContentDigest {
    /// Creates a fixed-width digest.
    #[must_use]
    pub const fn new(bytes: [u8; RERANK_CONTENT_DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; RERANK_CONTENT_DIGEST_BYTES] {
        &self.0
    }
}

/// One bounded rank-fusion contribution retained for rerank provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankContribution {
    profile: ProfileName,
    rank: usize,
    native_score: f64,
    weight: f64,
    contribution: f64,
}

impl RerankContribution {
    /// Creates validated fusion evidence.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an invalid profile, zero rank,
    /// non-finite score/contribution, or non-positive weight.
    pub fn new(
        profile: impl Into<String>,
        rank: usize,
        native_score: f64,
        weight: f64,
        contribution: f64,
    ) -> Result<Self> {
        let profile = ProfileName::new(profile.into()).map_err(|_| {
            invalid(
                "rerank_contribution_profile",
                "must be a bounded control-free profile name",
            )
        })?;
        if rank == 0 {
            return Err(invalid("rerank_contribution_rank", "must be positive"));
        }
        if !native_score.is_finite() {
            return Err(invalid(
                "rerank_contribution_native_score",
                "must be finite",
            ));
        }
        if !weight.is_finite() || weight <= 0.0 {
            return Err(invalid(
                "rerank_contribution_weight",
                "must be finite and positive",
            ));
        }
        if !contribution.is_finite() || contribution < 0.0 {
            return Err(invalid(
                "rerank_contribution",
                "must be finite and nonnegative",
            ));
        }
        Ok(Self {
            profile,
            rank,
            native_score,
            weight,
            contribution,
        })
    }

    /// Returns the contributing profile.
    #[must_use]
    pub fn profile(&self) -> &str {
        self.profile.as_str()
    }

    /// Returns its one-based rank.
    #[must_use]
    pub const fn rank(&self) -> usize {
        self.rank
    }

    /// Returns its diagnostic native score.
    #[must_use]
    pub const fn native_score(&self) -> f64 {
        self.native_score
    }

    /// Returns its declared fusion weight.
    #[must_use]
    pub const fn weight(&self) -> f64 {
        self.weight
    }

    /// Returns its weighted RRF contribution.
    #[must_use]
    pub const fn contribution(&self) -> f64 {
        self.contribution
    }
}

/// Correlates one rerank response with the request that produced it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RerankRequestId(u64);

impl RerankRequestId {
    /// Creates a nonzero request identity.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for the reserved zero value.
    pub fn new(id: u64) -> Result<Self> {
        if id == 0 {
            return Err(invalid("rerank_request_id", "must be nonzero"));
        }
        Ok(Self(id))
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One allow-listed metadata pair released to a reranker.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RerankMetadata {
    key: String,
    value: String,
}

impl RerankMetadata {
    /// Validates a borrowed metadata pair without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the key is empty or either
    /// side exceeds [`MAX_RERANK_METADATA_BYTES`].
    pub fn validate(key: &str, value: &str) -> Result<()> {
        if key.is_empty() || key.len() > MAX_RERANK_METADATA_BYTES {
            return Err(invalid(
                "rerank_metadata_key",
                "must contain 1..=1024 bytes",
            ));
        }
        if value.len() > MAX_RERANK_METADATA_BYTES {
            return Err(invalid("rerank_metadata_value", "exceeds 1024 bytes"));
        }
        Ok(())
    }

    /// Creates a bounded metadata pair.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the key is empty or either
    /// side exceeds [`MAX_RERANK_METADATA_BYTES`].
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let key = key.into();
        let value = value.into();
        Self::validate(&key, &value)?;
        Ok(Self { key, value })
    }

    /// Returns the metadata key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the metadata value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// One authorized candidate released to a reranker.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankCandidate {
    occurrence_id: OccurrenceId,
    point_id: PointId,
    source_version: SourceVersion,
    content_digest: RerankContentDigest,
    text: String,
    fused_rank: usize,
    fused_score: f64,
    contributions: Box<[RerankContribution]>,
    metadata: Box<[RerankMetadata]>,
}

impl RerankCandidate {
    /// Creates a bounded authorized candidate.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when text, rank, score, fusion
    /// provenance, or metadata violates its bound. Duplicate metadata keys and
    /// profile contributions are rejected rather than last-wins.
    #[allow(
        clippy::too_many_arguments,
        reason = "the envelope constructor makes every independently validated trust-boundary field explicit"
    )]
    pub fn new(
        occurrence_id: OccurrenceId,
        point_id: PointId,
        source_version: SourceVersion,
        content_digest: RerankContentDigest,
        text: impl Into<String>,
        fused_rank: usize,
        fused_score: f64,
        contributions: Vec<RerankContribution>,
        metadata: Vec<RerankMetadata>,
    ) -> Result<Self> {
        let text = text.into();
        if text.len() > MAX_RERANK_TEXT_BYTES {
            return Err(invalid(
                "rerank_candidate_text",
                "exceeds the authorized byte budget",
            ));
        }
        if fused_rank == 0 {
            return Err(invalid("rerank_fused_rank", "must be positive"));
        }
        if !fused_score.is_finite() {
            return Err(invalid("rerank_fused_score", "must be finite"));
        }
        if contributions.is_empty() || contributions.len() > MAX_RERANK_CONTRIBUTIONS {
            return Err(invalid(
                "rerank_candidate_contributions",
                "must contain 1..=127 entries",
            ));
        }
        let contribution_profiles = contributions
            .iter()
            .map(RerankContribution::profile)
            .collect::<BTreeSet<_>>();
        if contribution_profiles.len() != contributions.len() {
            return Err(invalid(
                "rerank_candidate_contributions",
                "must not repeat a profile",
            ));
        }
        if metadata.len() > MAX_RERANK_METADATA_ENTRIES {
            return Err(invalid(
                "rerank_candidate_metadata",
                "exceeds the allow-listed entry budget",
            ));
        }
        let unique = metadata
            .iter()
            .map(RerankMetadata::key)
            .collect::<BTreeSet<_>>();
        if unique.len() != metadata.len() {
            return Err(invalid(
                "rerank_candidate_metadata",
                "must not repeat a key",
            ));
        }
        Ok(Self {
            occurrence_id,
            point_id,
            source_version,
            content_digest,
            text,
            fused_rank,
            fused_score,
            contributions: contributions.into_boxed_slice(),
            metadata: metadata.into_boxed_slice(),
        })
    }

    /// Returns the stable occurrence identity.
    #[must_use]
    pub const fn occurrence_id(&self) -> OccurrenceId {
        self.occurrence_id
    }

    /// Returns the logical point identity.
    #[must_use]
    pub const fn point_id(&self) -> PointId {
        self.point_id
    }

    /// Returns the source version observed when the envelope was built.
    #[must_use]
    pub const fn source_version(&self) -> SourceVersion {
        self.source_version
    }

    /// Returns the authoritative text digest.
    #[must_use]
    pub const fn content_digest(&self) -> RerankContentDigest {
        self.content_digest
    }

    /// Returns the authorized text released to the provider.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the one-based fused rank before semantic reranking.
    #[must_use]
    pub const fn fused_rank(&self) -> usize {
        self.fused_rank
    }

    /// Returns the fused score retained for fallback and provenance.
    #[must_use]
    pub const fn fused_score(&self) -> f64 {
        self.fused_score
    }

    /// Returns the rank-fusion contribution evidence.
    #[must_use]
    pub fn contributions(&self) -> &[RerankContribution] {
        &self.contributions
    }

    /// Returns the allow-listed metadata released to the provider.
    #[must_use]
    pub fn metadata(&self) -> &[RerankMetadata] {
        &self.metadata
    }

    /// Returns the conservative extension-owned allocation projection.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::ArithmeticOverflow`] when the projection cannot
    /// be represented by the current platform.
    pub fn projected_bytes(&self) -> Result<usize> {
        let metadata_bytes = self.metadata.iter().try_fold(0_usize, |total, entry| {
            total
                .checked_add(size_of::<RerankMetadata>())
                .and_then(|value| value.checked_add(entry.key.capacity()))
                .and_then(|value| value.checked_add(entry.value.capacity()))
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "rerank_candidate_metadata_projection",
                })
        })?;
        let contribution_bytes =
            self.contributions
                .iter()
                .try_fold(0_usize, |total, contribution| {
                    total
                        .checked_add(contribution.profile.allocation_capacity_bytes())
                        .and_then(|value| value.checked_add(size_of::<RerankContribution>()))
                        .ok_or(QueryError::ArithmeticOverflow {
                            operation: "rerank_candidate_contribution_projection",
                        })
                })?;
        self.text
            .capacity()
            .checked_add(metadata_bytes)
            .and_then(|value| value.checked_add(contribution_bytes))
            .and_then(|value| value.checked_add(size_of::<Self>()))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_candidate_byte_projection",
            })
    }
}

/// A bounded, versioned rerank request.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankRequest {
    version: u16,
    request_id: RerankRequestId,
    model: RerankModelName,
    model_revision: u64,
    expires_at_micros: u64,
    query: RerankQuery,
    candidates: Box<[RerankCandidate]>,
    projected_bytes: usize,
}

impl RerankRequest {
    /// Creates a validated rerank request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the candidate set is empty or
    /// oversized, repeats an occurrence, exceeds the total authorized byte
    /// budget, or declares a zero model revision.
    pub fn new(
        request_id: RerankRequestId,
        model: RerankModelName,
        model_revision: u64,
        expires_at_micros: u64,
        query: RerankQuery,
        candidates: Vec<RerankCandidate>,
    ) -> Result<Self> {
        if model_revision == 0 {
            return Err(invalid("rerank_model_revision", "must be positive"));
        }
        if candidates.is_empty() || candidates.len() > MAX_RERANK_CANDIDATES {
            return Err(invalid(
                "rerank_candidates",
                "must contain 1..=512 candidates",
            ));
        }
        let unique_occurrences = candidates
            .iter()
            .map(RerankCandidate::occurrence_id)
            .collect::<BTreeSet<_>>();
        if unique_occurrences.len() != candidates.len() {
            return Err(invalid(
                "rerank_candidates",
                "must not repeat an occurrence",
            ));
        }
        let unique_points = candidates
            .iter()
            .map(RerankCandidate::point_id)
            .collect::<BTreeSet<_>>();
        if unique_points.len() != candidates.len() {
            return Err(invalid("rerank_candidates", "must not repeat a point"));
        }
        let candidate_bytes = candidates.iter().try_fold(0_usize, |total, candidate| {
            total
                .checked_add(candidate.projected_bytes()?)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "rerank_request_byte_projection",
                })
        })?;
        let projected_bytes = candidate_bytes
            .checked_add(query.0.capacity())
            .and_then(|value| value.checked_add(model.0.capacity()))
            .and_then(|value| value.checked_add(size_of::<Self>()))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_request_byte_projection",
            })?;
        if projected_bytes > MAX_RERANK_REQUEST_BYTES {
            return Err(invalid(
                "rerank_candidates",
                "exceed the complete request byte budget",
            ));
        }
        Ok(Self {
            version: RERANK_ENVELOPE_VERSION,
            request_id,
            model,
            model_revision,
            expires_at_micros,
            query,
            candidates: candidates.into_boxed_slice(),
            projected_bytes,
        })
    }

    /// Returns the envelope contract version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Returns the request identity.
    #[must_use]
    pub const fn request_id(&self) -> RerankRequestId {
        self.request_id
    }

    /// Rebinds a validated provisional request to its durable request identity.
    ///
    /// Candidate text and provenance remain owned by this request and are not
    /// cloned while PostgreSQL assigns the final identity.
    #[must_use]
    pub fn with_request_id(mut self, request_id: RerankRequestId) -> Self {
        self.request_id = request_id;
        self
    }

    /// Returns the immutable model identity.
    #[must_use]
    pub const fn model(&self) -> &RerankModelName {
        &self.model
    }

    /// Returns the required model revision.
    #[must_use]
    pub const fn model_revision(&self) -> u64 {
        self.model_revision
    }

    /// Returns the wall-clock expiry, in the caller's microsecond epoch.
    #[must_use]
    pub const fn expires_at_micros(&self) -> u64 {
        self.expires_at_micros
    }

    /// Returns the original query released to the provider.
    #[must_use]
    pub const fn query(&self) -> &RerankQuery {
        &self.query
    }

    /// Returns the authorized candidates.
    #[must_use]
    pub fn candidates(&self) -> &[RerankCandidate] {
        &self.candidates
    }

    /// Returns the conservative extension-owned request allocation projection.
    #[must_use]
    pub const fn projected_bytes(&self) -> usize {
        self.projected_bytes
    }
}

/// One scored row returned by a provider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RerankScore {
    occurrence_id: OccurrenceId,
    score: f64,
}

impl RerankScore {
    /// Creates a provider-supplied score.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a non-finite score. NaN would
    /// make the final ordering non-deterministic, so it is refused at the
    /// boundary rather than sorted around.
    pub fn new(occurrence_id: OccurrenceId, score: f64) -> Result<Self> {
        if !score.is_finite() {
            return Err(invalid("rerank_score", "must be finite"));
        }
        Ok(Self {
            occurrence_id,
            score,
        })
    }

    /// Returns the occurrence this score applies to.
    #[must_use]
    pub const fn occurrence_id(self) -> OccurrenceId {
        self.occurrence_id
    }

    /// Returns the provider score.
    #[must_use]
    pub const fn score(self) -> f64 {
        self.score
    }
}

/// A provider response, before validation.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankResponse {
    version: u16,
    request_id: RerankRequestId,
    model: RerankModelName,
    model_revision: u64,
    scores: Vec<RerankScore>,
}

impl RerankResponse {
    /// Records a raw provider response.
    #[must_use]
    pub const fn new(
        version: u16,
        request_id: RerankRequestId,
        model: RerankModelName,
        model_revision: u64,
        scores: Vec<RerankScore>,
    ) -> Self {
        Self {
            version,
            request_id,
            model,
            model_revision,
            scores,
        }
    }

    /// Returns the scores exactly as the provider supplied them.
    #[must_use]
    pub fn scores(&self) -> &[RerankScore] {
        &self.scores
    }

    /// Returns the immutable model identity echoed by the provider.
    #[must_use]
    pub const fn model(&self) -> &RerankModelName {
        &self.model
    }
}

/// What to do when a reranker cannot produce a usable response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RerankFallbackPolicy {
    /// Fail the query rather than return an un-reranked answer.
    Require,
    /// Report the attempt as non-authoritative instead of failing the query.
    ///
    /// The reranking port has no channel for "these rows are usable but
    /// unranked", so the executor surfaces a degraded attempt as a
    /// budget-exhausted completion. This policy chooses between a visibly
    /// incomplete answer and a hard error — not between two orderings.
    DegradeWithoutRerank,
}

/// Whether a provider must score every released candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RerankResponsePolicy {
    /// Refuse a response that omits any released candidate.
    RequireComplete,
    /// Admit an explicitly partial response for a caller-owned partial policy.
    AllowPartial,
}

/// Completeness of a validated response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RerankResponseCompletion {
    /// Every released candidate has exactly one score.
    Complete,
    /// At least one released candidate has no score.
    Partial,
}

/// Provider scores after request membership and completeness validation.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedRerankResponse {
    scores: Vec<RerankScore>,
    completion: RerankResponseCompletion,
}

impl ValidatedRerankResponse {
    /// Returns the accepted provider scores in provider order.
    #[must_use]
    pub fn scores(&self) -> &[RerankScore] {
        &self.scores
    }

    /// Returns whether every released candidate was scored.
    #[must_use]
    pub const fn completion(&self) -> RerankResponseCompletion {
        self.completion
    }

    /// Consumes the response and returns canonical relevance order.
    ///
    /// Reranker scores are higher-is-better. Provider array order is never
    /// trusted; equal scores break by ascending occurrence identity.
    #[must_use]
    pub fn into_ordered_scores(mut self) -> Vec<RerankScore> {
        self.scores.sort_by(|left, right| {
            right
                .score()
                .total_cmp(&left.score())
                .then_with(|| left.occurrence_id().cmp(&right.occurrence_id()))
        });
        self.scores
    }
}

/// Why a rerank response was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RerankRejection {
    /// The provider answered a different envelope version.
    VersionMismatch,
    /// The provider answered a different request.
    RequestMismatch,
    /// The provider answered with a different model identity.
    ModelMismatch,
    /// The provider answered with a different model revision.
    ModelRevisionMismatch,
    /// The request expired before the response arrived.
    Expired,
    /// The provider returned more scores than the request had candidates.
    TooManyScores,
    /// The provider scored the same occurrence twice.
    DuplicateOccurrence,
    /// The provider scored an occurrence the request never released.
    UnknownOccurrence,
    /// The response omitted a released candidate under a complete policy.
    Incomplete,
}

impl RerankRejection {
    /// Returns the stable diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::VersionMismatch => "version_mismatch",
            Self::RequestMismatch => "request_mismatch",
            Self::ModelMismatch => "model_mismatch",
            Self::ModelRevisionMismatch => "model_revision_mismatch",
            Self::Expired => "expired",
            Self::TooManyScores => "too_many_scores",
            Self::DuplicateOccurrence => "duplicate_occurrence",
            Self::UnknownOccurrence => "unknown_occurrence",
            Self::Incomplete => "incomplete",
        }
    }
}

/// Validates an untrusted provider response against the request it answers.
///
/// Returns the accepted scores in provider order, or the first reason the
/// response was refused. Every check compares against the *request*, never
/// against provider-supplied context, so a hostile provider cannot widen what
/// it is allowed to influence.
///
/// This validates the envelope only. The executor still re-reads every surviving
/// row under current MVCC, RLS, and filters before the answer is returned, so a
/// row whose source changed or whose visibility was revoked between the request
/// and the response is dropped regardless of what the provider said.
///
/// # Errors
///
/// This function does not error; a refusal is a [`RerankRejection`] value so
/// the caller can apply its [`RerankFallbackPolicy`].
pub fn validate_rerank_response(
    request: &RerankRequest,
    response: &RerankResponse,
    now_micros: u64,
) -> core::result::Result<Vec<RerankScore>, RerankRejection> {
    validate_rerank_response_with_policy(
        request,
        response,
        now_micros,
        RerankResponsePolicy::AllowPartial,
    )
    .map(|validated| validated.scores)
}

/// Validates an untrusted provider response and applies an explicit
/// completeness policy.
///
/// # Errors
///
/// Returns [`RerankRejection`] when identities, membership, expiry,
/// uniqueness, count, or requested completeness do not match the request.
pub fn validate_rerank_response_with_policy(
    request: &RerankRequest,
    response: &RerankResponse,
    now_micros: u64,
    policy: RerankResponsePolicy,
) -> core::result::Result<ValidatedRerankResponse, RerankRejection> {
    if response.version != request.version {
        return Err(RerankRejection::VersionMismatch);
    }
    if response.request_id != request.request_id {
        return Err(RerankRejection::RequestMismatch);
    }
    if response.model != request.model {
        return Err(RerankRejection::ModelMismatch);
    }
    if response.model_revision != request.model_revision {
        return Err(RerankRejection::ModelRevisionMismatch);
    }
    if now_micros >= request.expires_at_micros {
        return Err(RerankRejection::Expired);
    }
    if response.scores.len() > request.candidates.len() {
        return Err(RerankRejection::TooManyScores);
    }
    let released = request
        .candidates
        .iter()
        .map(RerankCandidate::occurrence_id)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for score in &response.scores {
        if !released.contains(&score.occurrence_id()) {
            return Err(RerankRejection::UnknownOccurrence);
        }
        if !seen.insert(score.occurrence_id()) {
            return Err(RerankRejection::DuplicateOccurrence);
        }
    }
    let completion = if response.scores.len() == request.candidates.len() {
        RerankResponseCompletion::Complete
    } else {
        RerankResponseCompletion::Partial
    };
    if completion == RerankResponseCompletion::Partial
        && policy == RerankResponsePolicy::RequireComplete
    {
        return Err(RerankRejection::Incomplete);
    }
    Ok(ValidatedRerankResponse {
        scores: response.scores.clone(),
        completion,
    })
}

fn invalid(field: &'static str, reason: &'static str) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    include!("rerank_envelope/tests.rs");
}
