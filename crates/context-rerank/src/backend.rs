//! Backend port, batching, and the deterministic no-model backend.
//!
//! The port is deliberately synchronous. pgContext's caller is a PostgreSQL
//! backend process serving one query on one thread; it cannot yield to an async
//! runtime mid-query, so a backend implementation owns its own deadline and
//! returns [`RerankBackendError::Timeout`] rather than blocking past it.
//!
//! A backend returns [`WireRerankResponse`] — untrusted data — and nothing else.
//! Turning that into scores the executor may act on requires [`score_batch`],
//! which is the only path from wire data to validated scores. That is a type
//! boundary, not a convention: a backend has no way to produce a validated
//! score, so a caller cannot skip validation by accident.

use std::mem::size_of;

use context_query::{
    Cancellation, MAX_RERANK_CANDIDATES, MAX_RERANK_REQUEST_BYTES, QueryClock, RerankCandidate,
    RerankFallbackPolicy, RerankModelName, RerankQuery, RerankRejection, RerankRequest,
    RerankRequestId, RerankResponsePolicy, RerankScore, validate_rerank_response_with_policy,
};

use crate::{WireError, WireRerankResponse, wire::bound_diagnostic};

/// Maximum candidates sent to a backend in one call.
///
/// This equals the envelope's own candidate ceiling: batching exists to keep a
/// large candidate set within the contract, not to second-guess a provider's
/// preferred batch size. A backend that wants smaller batches splits again.
pub const MAX_RERANK_BATCH: usize = MAX_RERANK_CANDIDATES;

/// Why a rerank attempt did not produce usable scores.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RerankBackendError {
    /// The backend is not configured or not reachable.
    Unavailable,
    /// The backend did not answer within the deadline.
    Timeout,
    /// The query was cancelled or interrupted.
    Cancelled,
    /// The caller asked for a batching plan that cannot be built.
    ///
    /// This is a caller-side programming error, kept distinct from
    /// [`Self::Transport`] so telemetry can tell a bad configuration from an
    /// unreachable provider.
    InvalidPlan {
        /// Stable reason the plan was refused.
        reason: String,
    },
    /// The transport failed. The reason is provider-supplied and untrusted.
    Transport {
        /// Diagnostic detail, truncated to [`crate::MAX_RERANK_DIAGNOSTIC_BYTES`].
        reason: String,
    },
    /// The payload could not be converted into the validated contract.
    Wire(WireError),
    /// The response was well-formed but did not answer this request.
    Rejected(RerankRejection),
}

impl RerankBackendError {
    /// Builds a transport failure, bounding the provider-supplied reason.
    #[must_use]
    pub fn transport(reason: impl Into<String>) -> Self {
        Self::Transport {
            reason: bound_diagnostic(reason.into()),
        }
    }

    /// Returns a stable diagnostic name safe for logs and telemetry.
    #[must_use]
    pub const fn stable_name(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::InvalidPlan { .. } => "invalid_plan",
            Self::Transport { .. } => "transport",
            Self::Wire(_) => "wire",
            Self::Rejected(rejection) => rejection.stable_name(),
        }
    }

    /// Reports whether the caller's fallback policy may apply.
    ///
    /// Cancellation is not a provider failure: the query is being torn down, so
    /// there is nothing for a degraded result to be returned to.
    #[must_use]
    pub const fn is_degradable(&self) -> bool {
        self.failure_reason().is_some()
    }

    /// Maps an operational provider failure to the SQL finalization vocabulary.
    ///
    /// Caller-invalid, injected, malformed, and cancelled work deliberately has
    /// no mapping: those failures must never become fused fallback output.
    #[must_use]
    pub const fn failure_reason(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable => Some("unavailable"),
            Self::Timeout => Some("timeout"),
            Self::Transport { .. } => Some("crash"),
            Self::Rejected(RerankRejection::Incomplete) => Some("partial_output"),
            Self::Rejected(RerankRejection::Expired) => Some("expired"),
            Self::Cancelled | Self::InvalidPlan { .. } | Self::Wire(_) | Self::Rejected(_) => None,
        }
    }
}

impl core::fmt::Display for RerankBackendError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.stable_name())
    }
}

impl std::error::Error for RerankBackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Wire(error) => Some(error),
            _ => None,
        }
    }
}

impl From<WireError> for RerankBackendError {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

/// Result type for a backend call.
pub type BackendResult<T> = Result<T, RerankBackendError>;

/// A transport to one external cross-encoder.
///
/// Implementations are responsible for their own deadline: `deadline_micros` is
/// on the caller's clock, and a call that cannot finish by then must return
/// [`RerankBackendError::Timeout`] rather than overrun the query's budget.
pub trait RerankBackend {
    /// Returns the model revision this backend serves.
    ///
    /// The caller stamps this into the request, and a response carrying a
    /// different revision is refused: a backend that reloads weights mid-query
    /// must report a new revision rather than silently answer with the old one.
    fn model_revision(&self) -> u64;

    /// Scores one authorized batch, returning the provider's raw payload.
    ///
    /// # Errors
    ///
    /// Returns [`RerankBackendError`] when the backend is unreachable, exceeds
    /// the deadline, or returns a payload that cannot be parsed.
    fn score(
        &self,
        request: &RerankRequest,
        deadline_micros: u64,
    ) -> BackendResult<WireRerankResponse>;
}

/// Splits an authorized candidate set into contract-sized requests.
///
/// Each batch gets its own request identity, derived from `base_request_id` and
/// the batch ordinal, so a response can only ever be matched to the batch that
/// produced it. Identities are derived rather than reused because a provider
/// that replays batch 0's answer for batch 1 must be refused, not merged.
#[derive(Clone, Debug)]
pub struct RerankBatches {
    requests: Vec<RerankRequest>,
}

impl RerankBatches {
    /// Builds the batches for one candidate set.
    ///
    /// The candidate set is checked for repeated occurrences across the whole
    /// plan, not merely within a batch. Per-request validation alone would let
    /// batching release the same row twice — multiplying the authorized byte
    /// budget and letting the provider return two scores for one row, of which
    /// one silently wins.
    ///
    /// Batches are split by authorized bytes as well as by count. The envelope
    /// bounds a request at [`context_query::MAX_RERANK_REQUEST_BYTES`] while a
    /// single candidate may carry
    /// [`context_query::MAX_RERANK_TEXT_BYTES`], so `batch_size` candidates can
    /// be far past the byte ceiling; splitting on count alone would build a plan
    /// the contract rejects. A single candidate always fits, because its own
    /// ceiling is well below the request's.
    ///
    /// # Errors
    ///
    /// Returns [`RerankBackendError::InvalidPlan`] when the candidate set is
    /// empty or repeats an occurrence, when `batch_size` is outside
    /// `1..=`[`MAX_RERANK_BATCH`], when a derived request identity would
    /// overflow, or when a batch violates the envelope contract.
    pub fn plan(
        base_request_id: RerankRequestId,
        model: &RerankModelName,
        model_revision: u64,
        expires_at_micros: u64,
        query: &RerankQuery,
        candidates: Vec<RerankCandidate>,
        batch_size: usize,
    ) -> BackendResult<Self> {
        if batch_size == 0 || batch_size > MAX_RERANK_BATCH {
            return Err(invalid_plan(format!(
                "rerank batch size must be 1..={MAX_RERANK_BATCH}"
            )));
        }
        if candidates.is_empty() {
            return Err(invalid_plan("rerank candidates must not be empty"));
        }
        let mut unique_occurrences = candidates
            .iter()
            .map(RerankCandidate::occurrence_id)
            .collect::<Vec<_>>();
        unique_occurrences.sort_unstable();
        if unique_occurrences.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid_plan(
                "rerank candidates must not repeat an occurrence",
            ));
        }
        let mut unique_points = candidates
            .iter()
            .map(RerankCandidate::point_id)
            .collect::<Vec<_>>();
        unique_points.sort_unstable();
        if unique_points.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid_plan("rerank candidates must not repeat a point"));
        }

        let mut requests = Vec::new();
        // `None` once the identity space is used up, so the overflow is only an
        // error if another batch actually needs an identity.
        let mut next_id = Some(base_request_id.get());
        let fixed_bytes = size_of::<RerankRequest>()
            .checked_add(model.as_str().len())
            .and_then(|bytes| bytes.checked_add(query.as_str().len()))
            .ok_or_else(|| invalid_plan("rerank request byte projection overflow"))?;
        let mut batch: Vec<RerankCandidate> = Vec::new();
        let mut batch_bytes = fixed_bytes;
        for candidate in candidates {
            let bytes = candidate
                .projected_bytes()
                .map_err(|error| invalid_plan(error.to_string()))?;
            let projected_bytes = batch_bytes
                .checked_add(bytes)
                .ok_or_else(|| invalid_plan("rerank request byte projection overflow"))?;
            let would_overflow = !batch.is_empty()
                && (batch.len() == batch_size || projected_bytes > MAX_RERANK_REQUEST_BYTES);
            if would_overflow {
                requests.push(build_request(
                    &mut next_id,
                    model,
                    model_revision,
                    expires_at_micros,
                    query,
                    core::mem::take(&mut batch),
                )?);
                batch_bytes = fixed_bytes;
            }
            batch_bytes = batch_bytes
                .checked_add(bytes)
                .ok_or_else(|| invalid_plan("rerank request byte projection overflow"))?;
            batch.push(candidate);
        }
        if !batch.is_empty() {
            requests.push(build_request(
                &mut next_id,
                model,
                model_revision,
                expires_at_micros,
                query,
                batch,
            )?);
        }
        Ok(Self { requests })
    }

    /// Returns the planned requests in candidate order.
    #[must_use]
    pub fn requests(&self) -> &[RerankRequest] {
        &self.requests
    }
}

fn build_request(
    next_id: &mut Option<u64>,
    model: &RerankModelName,
    model_revision: u64,
    expires_at_micros: u64,
    query: &RerankQuery,
    candidates: Vec<RerankCandidate>,
) -> BackendResult<RerankRequest> {
    let id = next_id.ok_or_else(|| invalid_plan("rerank request identity overflow"))?;
    let request_id = RerankRequestId::new(id).map_err(|error| invalid_plan(error.to_string()))?;
    let request = RerankRequest::new(
        request_id,
        model.clone(),
        model_revision,
        expires_at_micros,
        query.clone(),
        candidates,
    )
    .map_err(|error| invalid_plan(error.to_string()))?;
    *next_id = id.checked_add(1);
    Ok(request)
}

fn invalid_plan(reason: impl Into<String>) -> RerankBackendError {
    RerankBackendError::InvalidPlan {
        reason: reason.into(),
    }
}

/// What a rerank attempt produced.
#[derive(Clone, Debug, PartialEq)]
pub enum RerankOutcome {
    /// The provider answered and every check passed.
    Reranked(Vec<RerankScore>),
    /// The attempt failed and the caller's policy allows continuing without it.
    ///
    /// This carries no ordering. What the caller does with a degraded attempt is
    /// the caller's business — [`crate::BackendReranker`] reports it to the
    /// executor as a non-authoritative page.
    Degraded {
        /// Stable reason the rerank was skipped.
        reason: &'static str,
    },
}

/// Scores one batch and validates the answer against the request.
///
/// The clock is read before dispatch and after the provider answers. The first
/// check prevents releasing a later batch after expiry; the second counts all
/// provider time against the same inclusive deadline.
///
/// # Errors
///
/// Returns [`RerankBackendError`] when the backend fails or the response does
/// not answer this exact request.
pub fn score_batch(
    backend: &impl RerankBackend,
    request: &RerankRequest,
    clock: &impl QueryClock,
) -> BackendResult<Vec<RerankScore>> {
    if backend.model_revision() != request.model_revision() {
        return Err(RerankBackendError::Rejected(
            RerankRejection::ModelRevisionMismatch,
        ));
    }
    if clock.now_micros() >= request.expires_at_micros() {
        return Err(RerankBackendError::Rejected(RerankRejection::Expired));
    }
    let wire = backend.score(request, request.expires_at_micros())?;
    let response = wire.to_response()?;
    validate_rerank_response_with_policy(
        request,
        &response,
        clock.now_micros(),
        RerankResponsePolicy::RequireComplete,
    )
    .map(|validated| validated.scores().to_vec())
    .map_err(RerankBackendError::Rejected)
}

/// Runs every batch, applying the caller's fallback policy to a failure.
///
/// Cancellation is checked before each batch through both halves of the
/// [`Cancellation`] port — PostgreSQL reports a cancelled query through
/// `check_interrupt`, not through `is_cancelled` — so a torn-down query stops
/// at a batch boundary instead of releasing further authorized text to the
/// provider. A cancellation is never degraded into a usable answer.
///
/// A partial success is not returned: if any batch fails, the whole attempt
/// does, because a half-reranked ordering is neither the provider's ranking nor
/// the fused one.
///
/// # Errors
///
/// Returns [`RerankBackendError`] when the query was cancelled, or when a batch
/// fails under [`RerankFallbackPolicy::Require`].
pub fn score_all(
    backend: &impl RerankBackend,
    batches: &RerankBatches,
    cancellation: &impl Cancellation,
    clock: &impl QueryClock,
    policy: RerankFallbackPolicy,
) -> BackendResult<RerankOutcome> {
    let mut scores = Vec::new();
    for request in batches.requests() {
        check_cancellation(cancellation)?;
        match score_batch(backend, request, clock) {
            Ok(batch) => scores.extend(batch),
            Err(error) => return degrade(error, policy),
        }
    }
    Ok(RerankOutcome::Reranked(scores))
}

fn check_cancellation(cancellation: &impl Cancellation) -> BackendResult<()> {
    cancellation
        .check_interrupt()
        .map_err(|_| RerankBackendError::Cancelled)?;
    if cancellation.is_cancelled() {
        return Err(RerankBackendError::Cancelled);
    }
    Ok(())
}

fn degrade(
    error: RerankBackendError,
    policy: RerankFallbackPolicy,
) -> BackendResult<RerankOutcome> {
    let reason = error.failure_reason();
    match policy {
        RerankFallbackPolicy::DegradeWithoutRerank if reason.is_some() => {
            let Some(reason) = reason else {
                return Err(error);
            };
            Ok(RerankOutcome::Degraded { reason })
        }
        _ => Err(error),
    }
}

/// A backend that needs no model, no network, and no weights.
///
/// It exists so the whole path — envelope, wire format, batching, validation,
/// fallback — is exercisable in CI and reproducible on any host. The score is a
/// stable function of the candidate text, which makes it useless for relevance
/// and perfect for asserting that ordering is carried through unchanged.
#[derive(Clone, Copy, Debug)]
pub struct DeterministicRerankBackend {
    model_revision: u64,
}

impl DeterministicRerankBackend {
    /// Creates the backend for one model revision.
    #[must_use]
    pub const fn new(model_revision: u64) -> Self {
        Self { model_revision }
    }

    /// Returns the score this backend assigns to a text.
    ///
    /// The result is always in `[0, 1)`.
    #[must_use]
    pub fn score_of(text: &str) -> f64 {
        // FNV-1a over the text, mapped into [0, 1). Written out rather than
        // taken from `DefaultHasher`, whose output is not stable across
        // releases; this backend's whole value is that it never changes.
        // `tests::the_deterministic_backend_matches_its_pinned_values` fails if
        // any constant here is touched.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // FNV-1a's final multiply propagates a last-byte change upward only a
        // little, so the top bits of two texts differing in their last
        // character stay nearly equal. Taken raw, "text 1" and "text 2" would
        // score within 1e-7 of each other and the fixture would rank almost-tied
        // rows — useless for asserting that an ordering survived. The splitmix64
        // finalizer avalanches that difference across the whole word.
        hash ^= hash >> 30;
        hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash ^= hash >> 27;
        hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
        hash ^= hash >> 31;

        // f64 represents every integer below 2^53 exactly, and dividing by a
        // power of two is exact, so the quotient is the ratio it looks like.
        const MANTISSA_BITS: u32 = 53;
        /// 2^53, written out because a cast would trip the precision lint.
        const SCALE: f64 = 9_007_199_254_740_992.0;
        #[allow(clippy::cast_precision_loss)]
        let numerator = (hash >> (u64::BITS - MANTISSA_BITS)) as f64;
        numerator / SCALE
    }
}

impl RerankBackend for DeterministicRerankBackend {
    fn model_revision(&self) -> u64 {
        self.model_revision
    }

    fn score(
        &self,
        request: &RerankRequest,
        _deadline_micros: u64,
    ) -> BackendResult<WireRerankResponse> {
        Ok(WireRerankResponse {
            version: request.version(),
            request_id: request.request_id().get(),
            model: request.model().as_str().to_owned(),
            model_revision: self.model_revision,
            scores: request
                .candidates()
                .iter()
                .map(|candidate| crate::WireRerankScore {
                    occurrence_id: candidate.occurrence_id().get(),
                    score: Self::score_of(candidate.text()),
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    include!("backend/tests.rs");
}
