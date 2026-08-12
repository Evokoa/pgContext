//! End-to-end checks of [`BackendReranker`] driven by the real query executor.
//!
//! The unit tests in `adapter` assert what the port returns. The executor then
//! re-sorts, re-checks, and truncates that page, so a port can be individually
//! correct and still produce the wrong answer. These tests assert the answer the
//! user actually receives.

#![allow(clippy::expect_used)]

use context_core::{OccurrenceId, PointId, SourceAuthority, SourceKey, SourceVersion};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateProvenance, CandidateSource,
    CandidateSourceKind, Completion, ExecutionBudget, FilterCandidateBatch, HydratedCandidate,
    PortBudget, QueryClock, QueryError, QueryExecutor, QueryIr, QueryKind,
    RERANK_CONTENT_DIGEST_BYTES, RecheckPage, RerankCandidate, RerankContentDigest,
    RerankContribution, RerankFallbackPolicy, RerankModelName, RerankQuery, RerankRequest,
    RerankRequestId, ScoreOrder, SourceReadiness, SourceRechecker, StageDiagnostic, TelemetrySink,
};
use pgcontext_worker::{
    AuthorizedRowSource, BackendReranker, BackendRerankerConfig, BackendResult,
    DeterministicRerankBackend, RerankBackend, RerankBackendError, WireRerankResponse,
};

const POINTS: [u64; 4] = [1, 2, 3, 4];
const MODEL_REVISION: u64 = 7;

fn model() -> RerankModelName {
    RerankModelName::new("fixture-v1").expect("model")
}

fn query_text() -> RerankQuery {
    RerankQuery::new("postgres retrieval").expect("query")
}

struct FixedSource;

impl CandidateSource for FixedSource {
    fn readiness(
        &mut self,
        _query: &QueryIr,
        _budget: PortBudget,
    ) -> Result<SourceReadiness, QueryError> {
        Ok(SourceReadiness::Ready)
    }

    fn candidates(
        &mut self,
        _query: &QueryIr,
        _filter: Option<&FilterCandidateBatch>,
        limit: usize,
        _budget: PortBudget,
    ) -> Result<CandidatePage, QueryError> {
        // Descending fused score, so the pre-rerank order is 1, 2, 3, 4.
        let rows = POINTS
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let rank = u32::try_from(index).expect("bounded fixture rank");
                candidate(*point, 1.0 - f64::from(rank) / 10.0)
            })
            .take(limit)
            .collect();
        Ok(CandidatePage::new(rows, true))
    }
}

struct ExactRechecker;

impl SourceRechecker for ExactRechecker {
    fn recheck(
        &mut self,
        _query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
        _budget: PortBudget,
    ) -> Result<RecheckPage, QueryError> {
        let rows = candidates
            .iter()
            .take(limit)
            .map(|candidate| {
                HydratedCandidate::new(
                    candidate.point_id(),
                    SourceKey::new(candidate.point_id().get().to_string())?,
                    candidate.approximate_score(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RecheckPage::new(rows, candidates.len()))
    }
}

#[derive(Default)]
struct Diagnostics(Vec<StageDiagnostic>);

impl TelemetrySink for Diagnostics {
    fn record(&mut self, diagnostic: &StageDiagnostic) -> Result<(), QueryError> {
        self.0.push(diagnostic.clone());
        Ok(())
    }
}

struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct FixedClock;

impl QueryClock for FixedClock {
    fn now_micros(&self) -> u64 {
        1_000
    }
}

/// Releases the point id as text, or withholds the listed points.
struct Authorizer {
    withheld: Vec<u64>,
}

impl AuthorizedRowSource for Authorizer {
    fn authorize(
        &mut self,
        row: &HydratedCandidate,
        _max_candidate_bytes: usize,
    ) -> Result<Option<RerankCandidate>, QueryError> {
        if self.withheld.contains(&row.point_id().get()) {
            return Ok(None);
        }
        let occurrence =
            OccurrenceId::new(row.point_id().get()).ok_or(QueryError::InvalidInput {
                field: "occurrence_id",
                reason: "must be nonzero".to_owned(),
            })?;
        let version = SourceVersion::new(1).ok_or(QueryError::InvalidInput {
            field: "source_version",
            reason: "must be nonzero".to_owned(),
        })?;
        RerankCandidate::new(
            occurrence,
            row.point_id(),
            version,
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            format!("row {}", row.point_id().get()),
            usize::try_from(row.point_id().get()).map_err(|_| QueryError::ArithmeticOverflow {
                operation: "rerank_fixture_rank",
            })?,
            row.score(),
            vec![RerankContribution::new(
                "fixture",
                1,
                row.score(),
                1.0,
                0.01,
            )?],
            Vec::new(),
        )
        .map(Some)
    }
}

struct UnavailableBackend;

impl RerankBackend for UnavailableBackend {
    fn model_revision(&self) -> u64 {
        MODEL_REVISION
    }

    fn score(
        &self,
        _request: &RerankRequest,
        _deadline_micros: u64,
    ) -> BackendResult<WireRerankResponse> {
        Err(RerankBackendError::Unavailable)
    }
}

fn candidate(point_id: u64, score: f64) -> Candidate {
    Candidate::new(
        PointId::new(point_id),
        score,
        CandidateProvenance::new(
            OccurrenceId::new(point_id).expect("nonzero point"),
            CandidateBranch::DenseAnn,
            CandidateSourceKind::Hnsw,
            ScoreOrder::HigherIsBetter,
            SourceAuthority::DerivedArtifact,
        ),
    )
    .expect("candidate fixture")
}

fn plan(limit: usize) -> QueryIr {
    let inner = QueryIr::nearest(
        None,
        vec![1.0, 1.0],
        ScoreOrder::HigherIsBetter,
        None,
        POINTS.len(),
    )
    .expect("branch query");
    QueryIr::new(
        QueryKind::ExternalRerank {
            query: Box::new(inner),
            model_revision: MODEL_REVISION,
        },
        ScoreOrder::HigherIsBetter,
        None,
        limit,
    )
    .expect("external rerank plan")
}

/// The order the deterministic backend puts these points in.
fn provider_order(points: &[u64]) -> Vec<u64> {
    let mut scored = points
        .iter()
        .map(|point| {
            (
                *point,
                DeterministicRerankBackend::score_of(&format!("row {point}")),
            )
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
    scored.into_iter().map(|(point, _)| point).collect()
}

fn execute<B: RerankBackend>(
    backend: &B,
    withheld: Vec<u64>,
    limit: usize,
) -> context_query::ExecutionOutcome {
    execute_result(backend, withheld, limit).expect("execution")
}

fn execute_result<B: RerankBackend>(
    backend: &B,
    withheld: Vec<u64>,
    limit: usize,
) -> context_query::Result<context_query::ExecutionOutcome> {
    let mut rows = Authorizer { withheld };
    let (cancellation, clock) = (NeverCancelled, FixedClock);
    let mut reranker = BackendReranker::new(
        backend,
        &mut rows,
        &cancellation,
        &clock,
        BackendRerankerConfig::new(
            model(),
            query_text(),
            RerankFallbackPolicy::Require,
            RerankRequestId::new(1).expect("request id"),
        ),
    );
    QueryExecutor::new(
        &mut FixedSource,
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .with_external_reranker(&mut reranker)
    .execute(
        &plan(limit),
        ExecutionBudget::new(64, 64, 64, 8, 2, 8).expect("budget"),
    )
}

#[test]
fn the_users_final_ordering_and_scores_are_the_providers() {
    // The executor re-sorts the port's page by score before returning it. If the
    // adapter passed the fused scores through, this would come back as
    // [1, 2, 3, 4] — the pre-rerank order — and the rerank would be a no-op.
    let outcome = execute(
        &DeterministicRerankBackend::new(MODEL_REVISION),
        Vec::new(),
        4,
    );

    assert_eq!(outcome.completion(), Completion::Complete);
    let actual = outcome
        .points()
        .iter()
        .map(|row| row.point_id().get())
        .collect::<Vec<_>>();
    let expected = provider_order(&POINTS);
    assert_eq!(actual, expected);
    assert_ne!(
        actual,
        POINTS.to_vec(),
        "the fixture must not coincide with the fused order, or it proves nothing"
    );

    // The score the user sees is the reranker's relevance, so a threshold or
    // formula above this stage still reads a meaningful value.
    let scores = outcome
        .points()
        .iter()
        .map(|row| row.score())
        .collect::<Vec<_>>();
    let expected_scores = actual
        .iter()
        .map(|point| DeterministicRerankBackend::score_of(&format!("row {point}")))
        .collect::<Vec<_>>();
    assert_eq!(scores, expected_scores);
    assert!(scores.iter().all(|score| (0.0..1.0).contains(score)));
}

#[test]
fn a_score_threshold_above_a_rerank_filters_on_the_reranker_score() {
    // A rank ordinal in the score field would make this stage meaningless: any
    // positive floor would wipe the whole answer.
    let inner = QueryIr::nearest(
        None,
        vec![1.0, 1.0],
        ScoreOrder::HigherIsBetter,
        None,
        POINTS.len(),
    )
    .expect("branch query");
    let reranked = QueryIr::new(
        QueryKind::ExternalRerank {
            query: Box::new(inner),
            model_revision: MODEL_REVISION,
        },
        ScoreOrder::HigherIsBetter,
        None,
        POINTS.len(),
    )
    .expect("external rerank plan");
    let floor = 0.5;
    let thresholded = QueryIr::new(
        QueryKind::ScoreThreshold {
            query: Box::new(reranked),
            minimum: Some(floor),
            maximum: None,
        },
        ScoreOrder::HigherIsBetter,
        None,
        POINTS.len(),
    )
    .expect("threshold plan");

    let backend = DeterministicRerankBackend::new(MODEL_REVISION);
    let mut rows = Authorizer {
        withheld: Vec::new(),
    };
    let (cancellation, clock) = (NeverCancelled, FixedClock);
    let mut reranker = BackendReranker::new(
        &backend,
        &mut rows,
        &cancellation,
        &clock,
        BackendRerankerConfig::new(
            model(),
            query_text(),
            RerankFallbackPolicy::Require,
            RerankRequestId::new(1).expect("request id"),
        ),
    );
    let outcome = QueryExecutor::new(
        &mut FixedSource,
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .with_external_reranker(&mut reranker)
    .execute(
        &thresholded,
        ExecutionBudget::new(64, 64, 64, 8, 2, 8).expect("budget"),
    )
    .expect("threshold execution");

    let expected = POINTS
        .iter()
        .filter(|point| DeterministicRerankBackend::score_of(&format!("row {point}")) >= floor)
        .count();
    assert!(expected > 0, "the fixture must keep something");
    assert!(expected < POINTS.len(), "the fixture must drop something");
    assert_eq!(outcome.points().len(), expected);
    assert!(outcome.points().iter().all(|row| row.score() >= floor));
}

#[test]
fn a_withheld_row_fails_the_query_instead_of_disappearing() {
    let error = execute_result(&DeterministicRerankBackend::new(MODEL_REVISION), vec![2], 4)
        .expect_err("authority withholding must fail closed");
    assert!(matches!(error, QueryError::PortFailure { .. }));
}

#[test]
fn a_withheld_row_fails_even_when_other_rows_could_fill_the_limit() {
    let error = execute_result(&DeterministicRerankBackend::new(MODEL_REVISION), vec![2], 3)
        .expect_err("authority withholding has no implicit partial policy");
    assert!(matches!(error, QueryError::PortFailure { .. }));
}

#[test]
fn a_truncating_limit_keeps_the_top_of_the_providers_ordering() {
    let outcome = execute(
        &DeterministicRerankBackend::new(MODEL_REVISION),
        Vec::new(),
        2,
    );

    assert_eq!(outcome.completion(), Completion::Complete);
    let actual = outcome
        .points()
        .iter()
        .map(|row| row.point_id().get())
        .collect::<Vec<_>>();
    assert_eq!(actual, provider_order(&POINTS)[..2]);
}

#[test]
fn a_dead_provider_under_the_require_policy_fails_the_query() {
    let mut rows = Authorizer {
        withheld: Vec::new(),
    };
    let (cancellation, clock) = (NeverCancelled, FixedClock);
    let backend = UnavailableBackend;
    let mut reranker = BackendReranker::new(
        &backend,
        &mut rows,
        &cancellation,
        &clock,
        BackendRerankerConfig::new(
            model(),
            query_text(),
            RerankFallbackPolicy::Require,
            RerankRequestId::new(1).expect("request id"),
        ),
    );
    let error = QueryExecutor::new(
        &mut FixedSource,
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .with_external_reranker(&mut reranker)
    .execute(
        &plan(4),
        ExecutionBudget::new(64, 64, 64, 8, 2, 8).expect("budget"),
    )
    .expect_err("fail closed");
    assert!(matches!(
        error,
        QueryError::PortFailure {
            stage: "external_rerank",
            ..
        }
    ));
}

#[test]
fn a_dead_provider_under_the_degrade_policy_completes_non_authoritatively() {
    // The port has no "usable but not reranked" channel, so a degraded attempt
    // surfaces as a non-Complete outcome rather than a fused ordering dressed up
    // as a reranked one.
    let mut rows = Authorizer {
        withheld: Vec::new(),
    };
    let (cancellation, clock) = (NeverCancelled, FixedClock);
    let backend = UnavailableBackend;
    let mut reranker = BackendReranker::new(
        &backend,
        &mut rows,
        &cancellation,
        &clock,
        BackendRerankerConfig::new(
            model(),
            query_text(),
            RerankFallbackPolicy::DegradeWithoutRerank,
            RerankRequestId::new(1).expect("request id"),
        ),
    );
    let outcome = QueryExecutor::new(
        &mut FixedSource,
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .with_external_reranker(&mut reranker)
    .execute(
        &plan(4),
        ExecutionBudget::new(64, 64, 64, 8, 2, 8).expect("budget"),
    )
    .expect("degraded execution");
    assert_ne!(
        outcome.completion(),
        Completion::Complete,
        "an un-reranked answer must not be reported as authoritative"
    );
    assert!(
        outcome.points().is_empty(),
        "the port has no channel for an un-reranked ordering, so the degraded \
         outcome is a visibly incomplete answer rather than a fused one"
    );
}
