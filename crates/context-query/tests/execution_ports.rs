//! Fake-adapter tests for the pure query execution boundary.

#![allow(clippy::expect_used)]

use std::{cell::Cell, rc::Rc};

use context_core::{PointId, SourceKey};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateSource, Completion,
    ExecutionBudget, ExecutionOutcome, ExecutionState, FilterCandidateBatch, FilterCandidateSource,
    HydratedCandidate, QueryError, QueryExecutor, QueryIr, ReadinessReason, ScoreOrder,
    SourceReadiness, SourceRechecker, StageDiagnostic, TelemetrySink,
};
use proptest::prelude::*;

#[derive(Default)]
struct FakeCandidateSource {
    readiness: SourceReadiness,
    page: CandidatePage,
    readiness_calls: usize,
    candidate_calls: usize,
    cancel_on_candidates: Option<Rc<Cell<bool>>>,
}

impl CandidateSource for FakeCandidateSource {
    fn readiness(&mut self, _query: &QueryIr) -> Result<SourceReadiness, QueryError> {
        self.readiness_calls += 1;
        Ok(self.readiness.clone())
    }

    fn candidates(
        &mut self,
        _query: &QueryIr,
        _filter: Option<&FilterCandidateBatch>,
        _limit: usize,
    ) -> Result<CandidatePage, QueryError> {
        self.candidate_calls += 1;
        if let Some(cancelled) = &self.cancel_on_candidates {
            cancelled.set(true);
        }
        Ok(self.page.clone())
    }
}

#[derive(Default)]
struct FakeFilterSource {
    batch: FilterCandidateBatch,
    calls: usize,
    cancel_on_call: Option<Rc<Cell<bool>>>,
}

impl FilterCandidateSource for FakeFilterSource {
    fn filter_candidates(
        &mut self,
        _query: &QueryIr,
        _limit: usize,
    ) -> Result<FilterCandidateBatch, QueryError> {
        self.calls += 1;
        if let Some(cancelled) = &self.cancel_on_call {
            cancelled.set(true);
        }
        Ok(self.batch.clone())
    }
}

#[derive(Default)]
struct FakeRechecker {
    rows: Vec<HydratedCandidate>,
    calls: usize,
    cancel_on_call: Option<Rc<Cell<bool>>>,
}

impl SourceRechecker for FakeRechecker {
    fn recheck(
        &mut self,
        _query: &QueryIr,
        _candidates: &[Candidate],
        _limit: usize,
    ) -> Result<Vec<HydratedCandidate>, QueryError> {
        self.calls += 1;
        if let Some(cancelled) = &self.cancel_on_call {
            cancelled.set(true);
        }
        Ok(self.rows.clone())
    }
}

#[derive(Default)]
struct FakeTelemetry {
    diagnostics: Vec<StageDiagnostic>,
    cancel_on_record: Option<Rc<Cell<bool>>>,
}

impl TelemetrySink for FakeTelemetry {
    fn record(&mut self, diagnostic: &StageDiagnostic) -> Result<(), QueryError> {
        self.diagnostics.push(diagnostic.clone());
        if let Some(cancelled) = &self.cancel_on_record {
            cancelled.set(true);
        }
        Ok(())
    }
}

struct CancelAfter {
    checks: Cell<usize>,
    cancel_at: usize,
}

impl CancelAfter {
    fn never() -> Self {
        Self {
            checks: Cell::new(0),
            cancel_at: usize::MAX,
        }
    }
}

impl Cancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        let check = self.checks.get();
        self.checks.set(check.saturating_add(1));
        check >= self.cancel_at
    }
}

struct SharedCancellation(Rc<Cell<bool>>);

impl Cancellation for SharedCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

fn query(with_filter: bool) -> QueryIr {
    query_with_order(with_filter, ScoreOrder::HigherIsBetter)
}

fn query_with_order(with_filter: bool, score_order: ScoreOrder) -> QueryIr {
    QueryIr::nearest(
        None,
        vec![1.0, 0.0],
        score_order,
        with_filter.then(|| {
            serde_json::json!({
                "must": [{"key": "tenant", "match": {"value": "acme"}}]
            })
        }),
        2,
    )
    .expect("query fixture should be valid")
}

fn budget() -> ExecutionBudget {
    ExecutionBudget::new(8, 8, 8, 8, 4, 4).expect("budget fixture should be valid")
}

fn candidate(point_id: u64, score: f64) -> Candidate {
    Candidate::new(PointId::new(point_id), score, CandidateBranch::DenseAnn)
        .expect("candidate fixture should be valid")
}

fn hydrated(point_id: u64, score: f64) -> HydratedCandidate {
    HydratedCandidate::new(
        PointId::new(point_id),
        SourceKey::new(point_id.to_string()).expect("source key should be valid"),
        score,
    )
    .expect("hydrated fixture should be valid")
}

#[test]
fn executor_runs_filter_candidates_recheck_and_deterministic_output() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(3, 0.7), candidate(1, 0.7)], true),
        ..Default::default()
    };
    let mut filter = FakeFilterSource {
        batch: FilterCandidateBatch::new(vec![PointId::new(1), PointId::new(3)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker {
        rows: vec![hydrated(3, 0.8), hydrated(1, 0.8)],
        ..Default::default()
    };
    let mut telemetry = FakeTelemetry::default();
    let cancellation = CancelAfter::never();

    let outcome = QueryExecutor::new(
        &mut candidates,
        Some(&mut filter),
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(&query(true), budget())
    .expect("execution should succeed");

    assert_eq!(outcome.state(), &ExecutionState::Ready);
    assert_eq!(outcome.completion(), Completion::Complete);
    assert_eq!(
        outcome
            .points()
            .iter()
            .map(|point| point.point_id().get())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(outcome.usage().filter_candidates(), 2);
    assert_eq!(outcome.usage().candidates(), 2);
    assert_eq!(outcome.usage().rechecks(), 2);
    assert_eq!(candidates.candidate_calls, 1);
    assert_eq!(filter.calls, 1);
    assert_eq!(rechecker.calls, 1);
    assert_eq!(telemetry.diagnostics.len(), 3);
}

#[test]
fn executor_returns_not_ready_without_requesting_candidates() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::NotReady {
            reason: ReadinessReason::GenerationMissing,
        },
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("not-ready is a typed outcome");

    assert!(matches!(outcome.state(), ExecutionState::NotReady { .. }));
    assert_eq!(candidates.candidate_calls, 0);
    assert_eq!(rechecker.calls, 0);
}

#[test]
fn default_source_readiness_fails_closed() {
    let mut candidates = FakeCandidateSource::default();
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("default readiness is a typed not-ready outcome");

    assert_eq!(
        outcome.state(),
        &ExecutionState::NotReady {
            reason: ReadinessReason::Uninitialized,
        }
    );
    assert_eq!(candidates.candidate_calls, 0);
}

#[test]
fn executor_returns_rebuild_required_without_serving_stale_state() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::RebuildRequired {
            reason: ReadinessReason::ConfigurationChanged,
        },
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("rebuild-required is a typed outcome");

    assert!(matches!(
        outcome.state(),
        ExecutionState::RebuildRequired { .. }
    ));
    assert_eq!(candidates.candidate_calls, 0);
}

#[test]
fn executor_stops_before_first_port_when_cancelled() {
    let mut candidates = FakeCandidateSource::default();
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();
    let cancellation = CancelAfter {
        checks: Cell::new(0),
        cancel_at: 0,
    };

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(&query(false), budget())
    .expect("cancellation is a typed outcome");

    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert_eq!(candidates.readiness_calls, 0);
}

#[test]
fn executor_skips_recheck_for_an_empty_candidate_stage() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(Vec::new(), true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("empty stages should succeed");

    assert!(outcome.points().is_empty());
    assert_eq!(rechecker.calls, 0);
}

#[test]
fn executor_rejects_candidate_sources_that_exceed_the_requested_budget() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(
            (0..9).map(|point_id| candidate(point_id, 0.5)).collect(),
            true,
        ),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let error = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect_err("over-budget port output should fail");

    assert!(matches!(
        error,
        QueryError::PortContractViolation {
            stage: "candidate_source",
            ..
        }
    ));
}

#[test]
fn executor_cancels_between_readiness_and_candidate_work() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();
    let cancellation = CancelAfter {
        checks: Cell::new(0),
        cancel_at: 1,
    };

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(&query(false), budget())
    .expect("cancellation is a typed outcome");

    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert_eq!(candidates.readiness_calls, 1);
    assert_eq!(candidates.candidate_calls, 0);
}

#[test]
fn executor_checks_cancellation_immediately_after_candidate_port_calls() {
    let cancelled = Rc::new(Cell::new(false));
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        cancel_on_candidates: Some(Rc::clone(&cancelled)),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();
    let cancellation = SharedCancellation(cancelled);

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(&query(false), budget())
    .expect("cancellation is a typed outcome");

    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert_eq!(rechecker.calls, 0);
    assert!(telemetry.diagnostics.is_empty());
}

#[test]
fn executor_checks_cancellation_after_filter_recheck_and_telemetry_ports() {
    let cancelled = Rc::new(Cell::new(false));
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        ..Default::default()
    };
    let mut filter = FakeFilterSource {
        batch: FilterCandidateBatch::new(vec![PointId::new(1)], true),
        cancel_on_call: Some(Rc::clone(&cancelled)),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();
    let outcome = QueryExecutor::new(
        &mut candidates,
        Some(&mut filter),
        &mut rechecker,
        &mut telemetry,
        &SharedCancellation(Rc::clone(&cancelled)),
    )
    .execute(&query(true), budget())
    .expect("filter cancellation is a typed outcome");
    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert_eq!(candidates.candidate_calls, 0);
    assert!(telemetry.diagnostics.is_empty());

    cancelled.set(false);
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker {
        rows: vec![hydrated(1, 0.5)],
        cancel_on_call: Some(Rc::clone(&cancelled)),
        ..Default::default()
    };
    let mut telemetry = FakeTelemetry::default();
    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &SharedCancellation(Rc::clone(&cancelled)),
    )
    .execute(&query(false), budget())
    .expect("recheck cancellation is a typed outcome");
    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert_eq!(telemetry.diagnostics.len(), 1);

    cancelled.set(false);
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry {
        cancel_on_record: Some(Rc::clone(&cancelled)),
        ..Default::default()
    };
    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &SharedCancellation(cancelled),
    )
    .execute(&query(false), budget())
    .expect("telemetry cancellation is a typed outcome");
    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert_eq!(rechecker.calls, 0);
    assert_eq!(telemetry.diagnostics.len(), 1);
}

#[test]
fn executor_returns_budget_exhausted_before_an_extra_stage() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();
    let one_stage =
        ExecutionBudget::new(8, 8, 8, 1, 4, 4).expect("one-stage budget should be valid");

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), one_stage)
    .expect("budget exhaustion is a typed outcome");

    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.usage().stages(), 1);
    assert_eq!(rechecker.calls, 0);
}

#[test]
fn partial_filter_pages_stop_before_incomplete_candidate_work() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        ..Default::default()
    };
    let mut filter = FakeFilterSource {
        batch: FilterCandidateBatch::new(vec![PointId::new(1)], false),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        Some(&mut filter),
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(true), budget())
    .expect("a partial filter page is a typed bounded outcome");

    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.usage().expansions(), 0);
    assert_eq!(candidates.candidate_calls, 0);
    assert_eq!(rechecker.calls, 0);
}

#[test]
fn partial_candidate_pages_never_report_complete() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], false),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker {
        rows: vec![hydrated(1, 0.5)],
        ..Default::default()
    };
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("a partial candidate page is a typed bounded outcome");

    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.usage().expansions(), 0);
    assert_eq!(outcome.points().len(), 1);
}

#[test]
fn truncated_authoritative_rechecks_never_report_complete() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5), candidate(2, 0.4)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker {
        rows: vec![hydrated(1, 0.5)],
        ..Default::default()
    };
    let mut telemetry = FakeTelemetry::default();
    let recheck_limited =
        ExecutionBudget::new(8, 8, 1, 8, 1, 4).expect("recheck budget should be valid");

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), recheck_limited)
    .expect("truncated rechecks are a typed bounded outcome");

    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.usage().rechecks(), 1);
    assert_eq!(outcome.points().len(), 1);
    assert_eq!(outcome.usage().expansions(), 0);
}

#[test]
fn executor_rejects_result_limits_above_budget_before_port_work() {
    let mut candidates = FakeCandidateSource::default();
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();
    let result_limited =
        ExecutionBudget::new(8, 8, 8, 8, 4, 1).expect("result budget should be valid");

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), result_limited)
    .expect("result budget exhaustion is a typed outcome");

    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(candidates.readiness_calls, 0);
}

#[test]
fn executor_accepts_empty_authoritative_recheck_survivors() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker::default();
    let mut telemetry = FakeTelemetry::default();

    let outcome = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("empty recheck survivors should succeed");

    assert_eq!(outcome.completion(), Completion::Complete);
    assert!(outcome.points().is_empty());
    assert_eq!(rechecker.calls, 1);
}

#[test]
fn executor_rejects_rechecked_points_outside_the_candidate_page() {
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(vec![candidate(1, 0.5)], true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker {
        rows: vec![hydrated(99, 0.5)],
        ..Default::default()
    };
    let mut telemetry = FakeTelemetry::default();

    let error = QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect_err("recheck output must be derived from the candidate page");

    assert_eq!(
        error,
        QueryError::UnexpectedPointId {
            stage: "source_rechecker",
            point_id: PointId::new(99),
        }
    );
}

#[test]
fn executor_honors_both_final_score_directions() {
    let run = |score_order| {
        let mut candidates = FakeCandidateSource {
            readiness: SourceReadiness::Ready,
            page: CandidatePage::new(vec![candidate(1, 0.2), candidate(2, 0.8)], true),
            ..Default::default()
        };
        let mut rechecker = FakeRechecker {
            rows: vec![hydrated(1, 0.2), hydrated(2, 0.8)],
            ..Default::default()
        };
        let mut telemetry = FakeTelemetry::default();
        QueryExecutor::new(
            &mut candidates,
            None,
            &mut rechecker,
            &mut telemetry,
            &CancelAfter::never(),
        )
        .execute(&query_with_order(false, score_order), budget())
        .expect("score ordering should execute")
        .points()[0]
            .point_id()
    };

    assert_eq!(run(ScoreOrder::LowerIsBetter), PointId::new(1));
    assert_eq!(run(ScoreOrder::HigherIsBetter), PointId::new(2));
}

fn execute_rows(rows: Vec<HydratedCandidate>) -> ExecutionOutcome {
    let candidate_rows = rows
        .iter()
        .map(|row| candidate(row.point_id().get(), row.score()))
        .collect();
    let mut candidates = FakeCandidateSource {
        readiness: SourceReadiness::Ready,
        page: CandidatePage::new(candidate_rows, true),
        ..Default::default()
    };
    let mut rechecker = FakeRechecker {
        rows,
        ..Default::default()
    };
    let mut telemetry = FakeTelemetry::default();
    QueryExecutor::new(
        &mut candidates,
        None,
        &mut rechecker,
        &mut telemetry,
        &CancelAfter::never(),
    )
    .execute(&query(false), budget())
    .expect("generated execution should succeed")
}

proptest! {
    #[test]
    fn identical_fake_pages_produce_identical_bounded_outcomes(
        raw_rows in prop::collection::vec((0_u16..16, 0_u16..1000), 0..8)
    ) {
        let rows = raw_rows
            .into_iter()
            .map(|(point_id, score)| hydrated(u64::from(point_id), f64::from(score) / 100.0))
            .collect::<Vec<_>>();
        let first = execute_rows(rows.clone());
        let second = execute_rows(rows);

        prop_assert_eq!(&first, &second);
        prop_assert!(first.points().len() <= 2);
        prop_assert!(first.usage().candidates() <= 8);
        prop_assert!(first.usage().rechecks() <= 8);
        prop_assert!(first.usage().stages() <= 8);
    }
}
