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

use std::collections::BTreeSet;

use context_query::{
    Cancellation, MAX_RERANK_CANDIDATES, MAX_RERANK_REQUEST_BYTES, QueryClock, RerankCandidate,
    RerankFallbackPolicy, RerankRejection, RerankRequest, RerankRequestId, RerankScore,
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
        !matches!(self, Self::Cancelled)
    }
}

impl core::fmt::Display for RerankBackendError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.stable_name())?;
        match self {
            Self::InvalidPlan { reason } | Self::Transport { reason } => {
                write!(formatter, ": {reason}")
            }
            Self::Wire(error) => write!(formatter, ": {error}"),
            _ => Ok(()),
        }
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
        model_revision: u64,
        expires_at_micros: u64,
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
        let unique = candidates
            .iter()
            .map(RerankCandidate::occurrence_id)
            .collect::<BTreeSet<_>>();
        if unique.len() != candidates.len() {
            return Err(invalid_plan(
                "rerank candidates must not repeat an occurrence",
            ));
        }

        let mut requests = Vec::new();
        // `None` once the identity space is used up, so the overflow is only an
        // error if another batch actually needs an identity.
        let mut next_id = Some(base_request_id.get());
        let mut batch: Vec<RerankCandidate> = Vec::new();
        let mut batch_bytes = 0_usize;
        for candidate in candidates {
            let bytes = candidate.text().len();
            let would_overflow = !batch.is_empty()
                && (batch.len() == batch_size
                    || batch_bytes.saturating_add(bytes) > MAX_RERANK_REQUEST_BYTES);
            if would_overflow {
                requests.push(build_request(
                    &mut next_id,
                    model_revision,
                    expires_at_micros,
                    core::mem::take(&mut batch),
                )?);
                batch_bytes = 0;
            }
            batch_bytes = batch_bytes.saturating_add(bytes);
            batch.push(candidate);
        }
        if !batch.is_empty() {
            requests.push(build_request(
                &mut next_id,
                model_revision,
                expires_at_micros,
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
    model_revision: u64,
    expires_at_micros: u64,
    candidates: Vec<RerankCandidate>,
) -> BackendResult<RerankRequest> {
    let id = next_id.ok_or_else(|| invalid_plan("rerank request identity overflow"))?;
    let request_id = RerankRequestId::new(id).map_err(|error| invalid_plan(error.to_string()))?;
    let request = RerankRequest::new(request_id, model_revision, expires_at_micros, candidates)
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
/// The clock is read *after* the provider answers, so time spent inside the
/// backend counts against the envelope's expiry. Reading it beforehand would
/// make the expiry decorative: a provider that stalls past the deadline would
/// still be validated against the moment the call started.
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
    let wire = backend.score(request, request.expires_at_micros())?;
    let response = wire.to_response()?;
    context_query::validate_rerank_response(request, &response, clock.now_micros())
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
    match policy {
        RerankFallbackPolicy::DegradeWithoutRerank if error.is_degradable() => {
            Ok(RerankOutcome::Degraded {
                reason: error.stable_name(),
            })
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

    use core::cell::Cell;

    use super::*;
    use context_core::{OccurrenceId, PointId, SourceVersion};
    use context_query::QueryError;

    /// A clock the test advances by hand, so elapsed time is not wall-clock.
    struct TestClock {
        now: Cell<u64>,
    }

    impl TestClock {
        const fn at(now: u64) -> Self {
            Self {
                now: Cell::new(now),
            }
        }

        fn advance(&self, micros: u64) {
            self.now.set(self.now.get() + micros);
        }
    }

    impl QueryClock for TestClock {
        fn now_micros(&self) -> u64 {
            self.now.get()
        }
    }

    struct NeverCancelled;

    impl Cancellation for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    /// Reports cancellation the way PostgreSQL does: through `check_interrupt`,
    /// with `is_cancelled` never becoming true.
    struct InterruptedLikePostgres;

    impl Cancellation for InterruptedLikePostgres {
        fn check_interrupt(&self) -> context_query::Result<()> {
            Err(QueryError::PortFailure {
                stage: "test_interrupt",
                message: "cancelled".to_owned(),
            })
        }

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    struct UnavailableBackend {
        model_revision: u64,
    }

    impl RerankBackend for UnavailableBackend {
        fn model_revision(&self) -> u64 {
            self.model_revision
        }

        fn score(
            &self,
            _request: &RerankRequest,
            _deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            Err(RerankBackendError::Unavailable)
        }
    }

    /// Counts calls so a test can assert a call was never issued.
    struct CountingBackend {
        calls: Cell<usize>,
    }

    impl CountingBackend {
        const fn new() -> Self {
            Self {
                calls: Cell::new(0),
            }
        }
    }

    impl RerankBackend for CountingBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            self.calls.set(self.calls.get() + 1);
            DeterministicRerankBackend::new(7).score(request, deadline_micros)
        }
    }

    /// Answers the request it was given with someone else's occurrence — the
    /// shape a confused or hostile backend actually produces.
    struct ImpostorBackend {
        model_revision: u64,
    }

    impl RerankBackend for ImpostorBackend {
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
                model_revision: self.model_revision,
                scores: vec![crate::WireRerankScore {
                    occurrence_id: 9_999,
                    score: 1.0,
                }],
            })
        }
    }

    /// Stalls past the envelope's expiry before answering.
    struct SlowBackend<'clock> {
        clock: &'clock TestClock,
        stall_micros: u64,
    }

    impl RerankBackend for SlowBackend<'_> {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            self.clock.advance(self.stall_micros);
            DeterministicRerankBackend::new(7).score(request, deadline_micros)
        }
    }

    fn candidate(index: u64) -> RerankCandidate {
        RerankCandidate::new(
            OccurrenceId::new(index).expect("occurrence"),
            PointId::new(index),
            SourceVersion::new(1).expect("version"),
            format!("candidate text {index}"),
            Vec::new(),
        )
        .expect("candidate")
    }

    fn candidates(count: u64) -> Vec<RerankCandidate> {
        (1..=count).map(candidate).collect()
    }

    fn plan(candidates: Vec<RerankCandidate>, batch_size: usize) -> BackendResult<RerankBatches> {
        RerankBatches::plan(
            RerankRequestId::new(100).expect("request id"),
            7,
            10_000,
            candidates,
            batch_size,
        )
    }

    fn batches(count: u64, batch_size: usize) -> RerankBatches {
        plan(candidates(count), batch_size).expect("plan")
    }

    #[test]
    fn batching_covers_every_candidate_exactly_once_with_distinct_identities() {
        let planned = batches(7, 3);
        assert_eq!(planned.requests().len(), 3);

        let identities = planned
            .requests()
            .iter()
            .map(|request| request.request_id().get())
            .collect::<BTreeSet<_>>();
        assert_eq!(identities, BTreeSet::from([100, 101, 102]));

        let covered = planned
            .requests()
            .iter()
            .flat_map(RerankRequest::candidates)
            .map(RerankCandidate::occurrence_id)
            .collect::<Vec<_>>();
        assert_eq!(covered.len(), 7, "no candidate may be dropped or repeated");
        assert_eq!(covered.iter().copied().collect::<BTreeSet<_>>().len(), 7);
    }

    #[test]
    fn an_occurrence_repeated_across_batches_is_refused() {
        // Each batch is individually valid, so only a plan-wide check catches
        // this. Releasing the row twice would multiply the authorized byte
        // budget and let the provider pick which of its two scores survives.
        let repeated = vec![candidate(1), candidate(2), candidate(1)];
        assert!(matches!(
            plan(repeated, 2),
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn an_empty_candidate_set_is_refused_rather_than_silently_succeeding() {
        // `chunks` yields nothing for an empty slice, so without this check the
        // plan would be empty and the whole rerank would report success with no
        // ordering at all.
        assert!(matches!(
            plan(Vec::new(), 4),
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn a_batch_is_split_on_bytes_as_well_as_on_count() {
        // 200 candidates of 25 KiB fit the 512-candidate ceiling and blow the
        // 4 MiB request ceiling, so a count-only split would build a plan the
        // envelope refuses.
        let text = "x".repeat(25 * 1024);
        let candidates = (1..=200_u64)
            .map(|index| {
                RerankCandidate::new(
                    OccurrenceId::new(index).expect("occurrence"),
                    PointId::new(index),
                    SourceVersion::new(1).expect("version"),
                    text.clone(),
                    Vec::new(),
                )
                .expect("candidate")
            })
            .collect::<Vec<_>>();

        let planned = plan(candidates, MAX_RERANK_BATCH).expect("plan");
        assert!(
            planned.requests().len() > 1,
            "the byte ceiling must force a split"
        );
        for request in planned.requests() {
            let bytes = request
                .candidates()
                .iter()
                .map(|candidate| candidate.text().len())
                .sum::<usize>();
            assert!(bytes <= MAX_RERANK_REQUEST_BYTES);
        }
        let covered = planned
            .requests()
            .iter()
            .flat_map(RerankRequest::candidates)
            .count();
        assert_eq!(covered, 200, "no candidate may be dropped by the split");
    }

    #[test]
    fn an_unusable_batch_size_is_refused_rather_than_clamped() {
        for size in [0, MAX_RERANK_BATCH + 1] {
            assert!(matches!(
                plan(candidates(2), size),
                Err(RerankBackendError::InvalidPlan { .. })
            ));
        }
    }

    #[test]
    fn a_derived_identity_that_would_overflow_fails_instead_of_wrapping() {
        let planned = RerankBatches::plan(
            RerankRequestId::new(u64::MAX).expect("request id"),
            7,
            10_000,
            candidates(4),
            2,
        );
        assert!(matches!(
            planned,
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn the_deterministic_backend_scores_every_batch() {
        let backend = DeterministicRerankBackend::new(7);
        let outcome = score_all(
            &backend,
            &batches(7, 3),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect("scored");
        let RerankOutcome::Reranked(scores) = outcome else {
            unreachable!("a healthy backend must not degrade")
        };
        assert_eq!(scores.len(), 7);
        assert!(scores.iter().all(|score| score.score().is_finite()));
    }

    #[test]
    fn the_deterministic_backend_matches_its_pinned_values() {
        // Pinned so that changing the offset basis, the prime, or the shift is
        // a test failure rather than a silent change to a "never changes" score.
        assert_eq!(
            DeterministicRerankBackend::score_of("candidate text 1"),
            0.039_453_922_731_895_41
        );
        assert_eq!(
            DeterministicRerankBackend::score_of("candidate text 2"),
            0.456_253_198_859_871_03
        );
        assert_eq!(
            DeterministicRerankBackend::score_of(""),
            0.957_673_425_242_056_1
        );
    }

    #[test]
    fn texts_differing_in_one_character_are_not_almost_tied() {
        // Without the finalizer these differ by ~1e-7, which makes the fixture
        // unable to demonstrate that an ordering was carried through.
        let first = DeterministicRerankBackend::score_of("candidate text 1");
        let second = DeterministicRerankBackend::score_of("candidate text 2");
        assert!(
            (first - second).abs() > 0.01,
            "{first} and {second} are too close to order meaningfully"
        );
    }

    #[test]
    fn the_deterministic_backend_stays_inside_the_unit_interval() {
        for index in 0..2_000_u64 {
            let score = DeterministicRerankBackend::score_of(&format!("row {index}"));
            assert!((0.0..1.0).contains(&score), "{score} escaped [0, 1)");
        }
    }

    #[test]
    fn a_model_revision_drift_is_refused_before_the_call_is_made() {
        let backend = CountingBackend::new();
        let planned = RerankBatches::plan(
            RerankRequestId::new(1).expect("request id"),
            8,
            10_000,
            candidates(2),
            2,
        )
        .expect("plan");
        let error =
            score_batch(&backend, &planned.requests()[0], &TestClock::at(0)).expect_err("refuse");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::ModelRevisionMismatch)
        );
        assert_eq!(backend.calls.get(), 0, "the provider must not be called");
    }

    #[test]
    fn a_provider_scoring_an_unreleased_row_is_refused() {
        let backend = ImpostorBackend { model_revision: 7 };
        let planned = batches(2, 2);
        let error =
            score_batch(&backend, &planned.requests()[0], &TestClock::at(0)).expect_err("refuse");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::UnknownOccurrence)
        );
    }

    #[test]
    fn time_spent_inside_the_backend_counts_against_the_expiry() {
        // The clock starts well inside the envelope's 10_000 expiry and the
        // provider stalls past it. Reading the clock before the call — the
        // obvious mistake — would accept this response.
        let clock = TestClock::at(1_000);
        let backend = SlowBackend {
            clock: &clock,
            stall_micros: 20_000,
        };
        let planned = batches(2, 2);
        let error = score_batch(&backend, &planned.requests()[0], &clock).expect_err("refuse");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::Expired)
        );
    }

    #[test]
    fn a_response_arriving_inside_the_expiry_is_accepted() {
        let clock = TestClock::at(1_000);
        let backend = SlowBackend {
            clock: &clock,
            stall_micros: 500,
        };
        let planned = batches(2, 2);
        let scores = score_batch(&backend, &planned.requests()[0], &clock).expect("accepted");
        assert_eq!(scores.len(), 2);
    }

    #[test]
    fn a_failure_fails_the_query_under_the_require_policy() {
        let backend = UnavailableBackend { model_revision: 7 };
        let error = score_all(
            &backend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect_err("fail closed");
        assert_eq!(error, RerankBackendError::Unavailable);
    }

    #[test]
    fn a_failure_degrades_with_a_stable_reason_under_the_fuse_policy() {
        let backend = UnavailableBackend { model_revision: 7 };
        let outcome = score_all(
            &backend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect("degrade");
        assert_eq!(
            outcome,
            RerankOutcome::Degraded {
                reason: "unavailable"
            }
        );
    }

    /// Answers the first batch and then goes away.
    struct FlakyBackend;

    impl RerankBackend for FlakyBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            if request.request_id().get() == 100 {
                return DeterministicRerankBackend::new(7).score(request, deadline_micros);
            }
            Err(RerankBackendError::Timeout)
        }
    }

    #[test]
    fn a_partial_failure_never_yields_a_half_reranked_ordering() {
        let error = score_all(
            &FlakyBackend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect_err("fail closed");
        assert_eq!(error, RerankBackendError::Timeout);
    }

    #[test]
    fn a_partial_failure_discards_the_batches_that_did_succeed() {
        let outcome = score_all(
            &FlakyBackend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect("degrade");
        assert_eq!(
            outcome,
            RerankOutcome::Degraded { reason: "timeout" },
            "the first batch's scores must not survive as a partial ordering"
        );
    }

    #[test]
    fn a_postgres_style_interrupt_stops_before_the_first_call() {
        let backend = CountingBackend::new();
        let error = score_all(
            &backend,
            &batches(4, 2),
            &InterruptedLikePostgres,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect_err("stop");
        assert_eq!(error, RerankBackendError::Cancelled);
        assert_eq!(
            backend.calls.get(),
            0,
            "no authorized text may be released after cancellation"
        );
    }

    #[test]
    fn cancellation_is_never_degraded_into_a_usable_answer() {
        let backend = CountingBackend::new();
        let error = score_all(
            &backend,
            &batches(4, 2),
            &InterruptedLikePostgres,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect_err("stop");
        assert_eq!(error, RerankBackendError::Cancelled);
        assert_eq!(backend.calls.get(), 0);
    }

    #[test]
    fn a_provider_diagnostic_cannot_flood_a_log() {
        let error = RerankBackendError::transport("!".repeat(10_000));
        let RerankBackendError::Transport { reason } = &error else {
            unreachable!("constructed a transport failure")
        };
        assert_eq!(reason.len(), crate::MAX_RERANK_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn bounding_a_diagnostic_never_splits_a_character() {
        let error = RerankBackendError::transport("é".repeat(1_000));
        let RerankBackendError::Transport { reason } = &error else {
            unreachable!("constructed a transport failure")
        };
        assert!(reason.len() <= crate::MAX_RERANK_DIAGNOSTIC_BYTES);
        assert!(reason.chars().all(|character| character == 'é'));
    }
}
