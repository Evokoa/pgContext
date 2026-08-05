//! Versioned envelopes for provider-neutral external reranking.
//!
//! An external cross-encoder is untrusted infrastructure. It receives a bounded,
//! explicitly authorized snapshot of already-visible rows and returns an
//! ordering; it never receives database authority. Everything it can influence
//! is re-validated against the request that produced it, and the executor
//! re-applies the authoritative source recheck afterwards.
//!
//! The envelope is versioned because the request and the response cross a
//! process boundary: a provider built against an older contract must be
//! rejected, not silently reinterpreted.

use std::collections::BTreeSet;

use context_core::{OccurrenceId, PointId, SourceVersion};

use crate::{QueryError, Result};

/// Envelope contract version. Bump when any field's meaning changes.
pub const RERANK_ENVELOPE_VERSION: u16 = 1;
/// Maximum candidates in one rerank request.
pub const MAX_RERANK_CANDIDATES: usize = 512;
/// Maximum authorized text bytes attached to one candidate.
pub const MAX_RERANK_TEXT_BYTES: usize = 32 * 1024;
/// Maximum authorized text bytes across one request.
pub const MAX_RERANK_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum allow-listed metadata entries on one candidate.
pub const MAX_RERANK_METADATA_ENTRIES: usize = 8;
/// Maximum bytes in one metadata key or value.
pub const MAX_RERANK_METADATA_BYTES: usize = 1_024;

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
    /// Creates a bounded metadata pair.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the key is empty or either
    /// side exceeds [`MAX_RERANK_METADATA_BYTES`].
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Result<Self> {
        let key = key.into();
        let value = value.into();
        if key.is_empty() || key.len() > MAX_RERANK_METADATA_BYTES {
            return Err(invalid(
                "rerank_metadata_key",
                "must contain 1..=1024 bytes",
            ));
        }
        if value.len() > MAX_RERANK_METADATA_BYTES {
            return Err(invalid("rerank_metadata_value", "exceeds 1024 bytes"));
        }
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
    text: String,
    metadata: Vec<RerankMetadata>,
}

impl RerankCandidate {
    /// Creates a bounded authorized candidate.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the text exceeds
    /// [`MAX_RERANK_TEXT_BYTES`], the metadata exceeds
    /// [`MAX_RERANK_METADATA_ENTRIES`], or a metadata key repeats. A duplicate
    /// key is rejected rather than last-wins so the provider cannot observe a
    /// pair the caller did not intend to release.
    pub fn new(
        occurrence_id: OccurrenceId,
        point_id: PointId,
        source_version: SourceVersion,
        text: impl Into<String>,
        metadata: Vec<RerankMetadata>,
    ) -> Result<Self> {
        let text = text.into();
        if text.len() > MAX_RERANK_TEXT_BYTES {
            return Err(invalid(
                "rerank_candidate_text",
                "exceeds the authorized byte budget",
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
            text,
            metadata,
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

    /// Returns the authorized text released to the provider.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the allow-listed metadata released to the provider.
    #[must_use]
    pub fn metadata(&self) -> &[RerankMetadata] {
        &self.metadata
    }
}

/// A bounded, versioned rerank request.
#[derive(Clone, Debug, PartialEq)]
pub struct RerankRequest {
    version: u16,
    request_id: RerankRequestId,
    model_revision: u64,
    expires_at_micros: u64,
    candidates: Vec<RerankCandidate>,
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
        model_revision: u64,
        expires_at_micros: u64,
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
        let unique = candidates
            .iter()
            .map(RerankCandidate::occurrence_id)
            .collect::<BTreeSet<_>>();
        if unique.len() != candidates.len() {
            return Err(invalid(
                "rerank_candidates",
                "must not repeat an occurrence",
            ));
        }
        let total_bytes = candidates
            .iter()
            .try_fold(0_usize, |total, candidate| {
                total.checked_add(candidate.text().len())
            })
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_request_byte_projection",
            })?;
        if total_bytes > MAX_RERANK_REQUEST_BYTES {
            return Err(invalid(
                "rerank_candidates",
                "exceed the authorized request byte budget",
            ));
        }
        Ok(Self {
            version: RERANK_ENVELOPE_VERSION,
            request_id,
            model_revision,
            expires_at_micros,
            candidates,
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

    /// Returns the authorized candidates.
    #[must_use]
    pub fn candidates(&self) -> &[RerankCandidate] {
        &self.candidates
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
    model_revision: u64,
    scores: Vec<RerankScore>,
}

impl RerankResponse {
    /// Records a raw provider response.
    #[must_use]
    pub const fn new(
        version: u16,
        request_id: RerankRequestId,
        model_revision: u64,
        scores: Vec<RerankScore>,
    ) -> Self {
        Self {
            version,
            request_id,
            model_revision,
            scores,
        }
    }

    /// Returns the scores exactly as the provider supplied them.
    #[must_use]
    pub fn scores(&self) -> &[RerankScore] {
        &self.scores
    }
}

/// What to do when a reranker cannot produce a usable response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RerankFallbackPolicy {
    /// Fail the query rather than return an un-reranked answer.
    Require,
    /// Return the fused pre-rerank ordering, marked degraded.
    FuseWithoutRerank,
}

/// Why a rerank response was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RerankRejection {
    /// The provider answered a different envelope version.
    VersionMismatch,
    /// The provider answered a different request.
    RequestMismatch,
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
}

impl RerankRejection {
    /// Returns the stable diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::VersionMismatch => "version_mismatch",
            Self::RequestMismatch => "request_mismatch",
            Self::ModelRevisionMismatch => "model_revision_mismatch",
            Self::Expired => "expired",
            Self::TooManyScores => "too_many_scores",
            Self::DuplicateOccurrence => "duplicate_occurrence",
            Self::UnknownOccurrence => "unknown_occurrence",
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
    if response.version != request.version {
        return Err(RerankRejection::VersionMismatch);
    }
    if response.request_id != request.request_id {
        return Err(RerankRejection::RequestMismatch);
    }
    if response.model_revision != request.model_revision {
        return Err(RerankRejection::ModelRevisionMismatch);
    }
    if now_micros > request.expires_at_micros {
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
    Ok(response.scores.clone())
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

    use super::*;

    fn occurrence(id: u64) -> OccurrenceId {
        OccurrenceId::new(id).expect("nonzero occurrence")
    }

    fn candidate(id: u64) -> RerankCandidate {
        RerankCandidate::new(
            occurrence(id),
            PointId::new(id),
            SourceVersion::new(1).expect("source version"),
            "authorized text",
            Vec::new(),
        )
        .expect("candidate")
    }

    fn request() -> RerankRequest {
        RerankRequest::new(
            RerankRequestId::new(7).expect("request id"),
            3,
            1_000,
            vec![candidate(1), candidate(2)],
        )
        .expect("request")
    }

    fn response(scores: Vec<RerankScore>) -> RerankResponse {
        RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(7).expect("request id"),
            3,
            scores,
        )
    }

    fn score(id: u64, value: f64) -> RerankScore {
        RerankScore::new(occurrence(id), value).expect("score")
    }

    #[test]
    fn identities_and_scores_reject_reserved_and_non_finite_values() {
        assert!(RerankRequestId::new(0).is_err());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                RerankScore::new(occurrence(1), value).is_err(),
                "score {value} must be rejected"
            );
        }
    }

    #[test]
    fn requests_bound_candidate_count_uniqueness_and_total_bytes() {
        assert!(
            RerankRequest::new(RerankRequestId::new(1).expect("id"), 1, 0, Vec::new()).is_err()
        );
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                0,
                0,
                vec![candidate(1)]
            )
            .is_err(),
            "a zero model revision must be rejected"
        );
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                1,
                0,
                vec![candidate(1), candidate(1)]
            )
            .is_err(),
            "a repeated occurrence must be rejected"
        );

        let oversized = (1..=MAX_RERANK_CANDIDATES + 1)
            .map(|id| candidate(id as u64))
            .collect::<Vec<_>>();
        assert!(RerankRequest::new(RerankRequestId::new(1).expect("id"), 1, 0, oversized).is_err());
    }

    #[test]
    fn candidates_bound_text_and_allow_listed_metadata() {
        assert!(
            RerankCandidate::new(
                occurrence(1),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                "x".repeat(MAX_RERANK_TEXT_BYTES + 1),
                Vec::new(),
            )
            .is_err()
        );
        let too_many = (0..=MAX_RERANK_METADATA_ENTRIES)
            .map(|index| RerankMetadata::new(format!("k{index}"), "v").expect("metadata"))
            .collect::<Vec<_>>();
        assert!(
            RerankCandidate::new(
                occurrence(1),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                "text",
                too_many,
            )
            .is_err()
        );
        let duplicated = vec![
            RerankMetadata::new("tenant", "a").expect("metadata"),
            RerankMetadata::new("tenant", "b").expect("metadata"),
        ];
        assert!(
            RerankCandidate::new(
                occurrence(1),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                "text",
                duplicated,
            )
            .is_err(),
            "a repeated metadata key must be rejected, not last-wins"
        );
        assert!(RerankMetadata::new("", "v").is_err());
    }

    #[test]
    fn a_well_formed_response_is_accepted_in_provider_order() {
        let accepted = validate_rerank_response(
            &request(),
            &response(vec![score(2, 0.9), score(1, 0.1)]),
            500,
        )
        .expect("well-formed response");
        assert_eq!(
            accepted
                .iter()
                .map(|score| score.occurrence_id())
                .collect::<Vec<_>>(),
            vec![occurrence(2), occurrence(1)]
        );
    }

    #[test]
    fn a_response_answering_a_different_request_is_refused() {
        let mismatched = RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(8).expect("id"),
            3,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &mismatched, 0),
            Err(RerankRejection::RequestMismatch)
        );
    }

    #[test]
    fn version_model_and_expiry_mismatches_are_each_refused() {
        let wrong_version = RerankResponse::new(
            RERANK_ENVELOPE_VERSION + 1,
            RerankRequestId::new(7).expect("id"),
            3,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &wrong_version, 0),
            Err(RerankRejection::VersionMismatch)
        );

        let wrong_model = RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(7).expect("id"),
            4,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &wrong_model, 0),
            Err(RerankRejection::ModelRevisionMismatch)
        );

        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(1, 1.0)]), 1_001),
            Err(RerankRejection::Expired)
        );
        assert!(
            validate_rerank_response(&request(), &response(vec![score(1, 1.0)]), 1_000).is_ok(),
            "expiry is inclusive of its own instant"
        );
    }

    #[test]
    fn injected_duplicated_and_oversized_score_sets_are_refused() {
        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(99, 1.0)]), 0),
            Err(RerankRejection::UnknownOccurrence),
            "a provider must not score an occurrence it was never given"
        );
        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(1, 1.0), score(1, 2.0)]), 0),
            Err(RerankRejection::DuplicateOccurrence)
        );
        assert_eq!(
            validate_rerank_response(
                &request(),
                &response(vec![score(1, 1.0), score(2, 1.0), score(1, 1.0)]),
                0
            ),
            Err(RerankRejection::TooManyScores)
        );
    }

    #[test]
    fn a_provider_may_return_fewer_scores_than_it_was_given() {
        let accepted = validate_rerank_response(&request(), &response(vec![score(1, 0.5)]), 0)
            .expect("a partial score set is well formed");
        assert_eq!(accepted.len(), 1);
    }

    #[test]
    fn rejections_and_policies_carry_stable_names() {
        assert_eq!(
            RerankRejection::UnknownOccurrence.stable_name(),
            "unknown_occurrence"
        );
        assert_ne!(
            RerankFallbackPolicy::Require,
            RerankFallbackPolicy::FuseWithoutRerank
        );
    }
}
