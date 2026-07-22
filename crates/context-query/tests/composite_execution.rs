//! Pure recursive composite executor tests.

#![allow(clippy::expect_used)]

use context_core::{PointId, SourceKey};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateSource, Completion,
    ExecutionBudget, ExecutionState, FilterCandidateBatch, FilterCandidateSource, Formula,
    HydratedCandidate, QueryError, QueryExecutor, QueryIr, QueryKind, ScoreOrder, SourceReadiness,
    SourceRechecker, StageDiagnostic, StageKind, TelemetrySink,
};
use std::cell::Cell;

#[derive(Default)]
struct RoutingSource {
    calls: usize,
    readiness_calls: usize,
    unavailable_second_branch: bool,
    partial_pages: bool,
}

impl CandidateSource for RoutingSource {
    fn readiness(&mut self, query: &QueryIr) -> Result<SourceReadiness, QueryError> {
        self.readiness_calls += 1;
        if self.unavailable_second_branch && is_second_branch(query) {
            Ok(SourceReadiness::NotReady {
                reason: context_query::ReadinessReason::GenerationMissing,
            })
        } else {
            Ok(SourceReadiness::Ready)
        }
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        _filter: Option<&FilterCandidateBatch>,
        limit: usize,
    ) -> Result<CandidatePage, QueryError> {
        self.calls += 1;
        let rows = if is_second_branch(query) {
            vec![candidate(2, 0.9), candidate(3, 0.1)]
        } else {
            vec![candidate(1, 0.9), candidate(2, 0.1)]
        };
        Ok(CandidatePage::new(
            rows.into_iter().take(limit).collect(),
            !self.partial_pages,
        ))
    }
}

#[derive(Default)]
struct ExactRechecker;

impl SourceRechecker for ExactRechecker {
    fn recheck(
        &mut self,
        _query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Vec<HydratedCandidate>, QueryError> {
        candidates
            .iter()
            .take(limit)
            .map(|candidate| {
                HydratedCandidate::new(
                    candidate.point_id(),
                    SourceKey::new(candidate.point_id().get().to_string())?,
                    candidate.score(),
                )
            })
            .collect()
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

#[derive(Default)]
struct CountingFilter {
    calls: usize,
}

impl FilterCandidateSource for CountingFilter {
    fn filter_candidates(
        &mut self,
        _query: &QueryIr,
        limit: usize,
    ) -> Result<FilterCandidateBatch, QueryError> {
        self.calls += 1;
        Ok(FilterCandidateBatch::new(
            (1..=limit as u64).map(PointId::new).collect(),
            true,
        ))
    }
}

struct CancelOnCall {
    calls: Cell<usize>,
    call: usize,
}

impl Cancellation for CancelOnCall {
    fn is_cancelled(&self) -> bool {
        let calls = self.calls.get() + 1;
        self.calls.set(calls);
        calls >= self.call
    }
}

fn candidate(point_id: u64, score: f64) -> Candidate {
    Candidate::new(PointId::new(point_id), score, CandidateBranch::DenseAnn)
        .expect("candidate fixture should be finite")
}

fn branch(first_dimension: f32) -> QueryIr {
    QueryIr::nearest(
        None,
        vec![first_dimension, 1.0],
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("branch query should be valid")
}

fn filtered_branch(first_dimension: f32) -> QueryIr {
    QueryIr::nearest(
        None,
        vec![first_dimension, 1.0],
        ScoreOrder::HigherIsBetter,
        Some(serde_json::json!({
            "must": [{"key": "tenant", "match": {"value": "acme"}}]
        })),
        3,
    )
    .expect("filtered branch query should be valid")
}

fn is_second_branch(query: &QueryIr) -> bool {
    matches!(
        query.kind(),
        QueryKind::Nearest { vector, .. } if vector.as_slice()[0] < 0.0
    )
}

fn budget(stages: usize) -> ExecutionBudget {
    ExecutionBudget::new(8, 8, 8, stages, 2, 3).expect("test budget should be valid")
}

fn execute(
    query: &QueryIr,
    source: &mut RoutingSource,
    stages: usize,
) -> context_query::ExecutionOutcome {
    QueryExecutor::new(
        source,
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .execute(query, budget(stages))
    .expect("composite execution should succeed")
}

#[test]
fn prefetch_uses_rrf_with_deduplication_and_deterministic_ties() {
    let query = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![branch(1.0), branch(-1.0)],
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("prefetch should be valid");
    let outcome = execute(&query, &mut RoutingSource::default(), 8);

    assert_eq!(outcome.completion(), Completion::Complete);
    assert_eq!(
        outcome
            .points()
            .iter()
            .map(|point| point.point_id().get())
            .collect::<Vec<_>>(),
        vec![2, 1, 3]
    );
    assert_eq!(outcome.usage().candidates(), 4);
    assert_eq!(outcome.usage().rechecks(), 4);
    assert!(
        outcome
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.stage() == StageKind::Fusion
                && diagnostic.strategy() == "reciprocal_rank_fusion")
    );
}

#[test]
fn weighted_prefetch_normalizes_branch_scores_and_weights() {
    let weighted = |query, weight| {
        QueryIr::new(
            QueryKind::Weighted {
                query: Box::new(query),
                weight,
            },
            ScoreOrder::HigherIsBetter,
            None,
            3,
        )
        .expect("weighted branch should be valid")
    };
    let query = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![weighted(branch(1.0), 3.0), weighted(branch(-1.0), 1.0)],
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("weighted prefetch should be valid");
    let outcome = execute(&query, &mut RoutingSource::default(), 8);

    assert_eq!(
        outcome
            .points()
            .iter()
            .map(|point| (point.point_id().get(), point.score()))
            .collect::<Vec<_>>(),
        vec![(1, 0.75), (2, 0.25), (3, 0.0)]
    );
    assert_eq!(
        outcome
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.strategy() == "weighted_score")
            .count(),
        2
    );
}

#[test]
fn prefetch_executes_direct_weighted_branch_limits() {
    let weighted = QueryIr::new(
        QueryKind::Weighted {
            query: Box::new(branch(1.0)),
            weight: 2.0,
        },
        ScoreOrder::HigherIsBetter,
        None,
        1,
    )
    .expect("weighted branch should be valid");
    let query = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![weighted],
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("prefetch should be valid");
    let outcome = execute(&query, &mut RoutingSource::default(), 8);

    assert_eq!(outcome.points().len(), 1);
    assert_eq!(outcome.points()[0].point_id().get(), 1);
    assert!(
        outcome
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.strategy() == "weighted_score")
    );
}

#[test]
fn formula_threshold_and_rerank_execute_in_tree_order() {
    let formula = QueryIr::new(
        QueryKind::Formula {
            query: Box::new(branch(1.0)),
            formula: Formula::new("$score * 2").expect("formula text should be bounded"),
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("formula node should be valid");
    let threshold = QueryIr::new(
        QueryKind::ScoreThreshold {
            query: Box::new(formula),
            minimum: Some(0.5),
            maximum: None,
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("threshold node should be valid");
    let query = QueryIr::new(
        QueryKind::Rerank {
            query: Box::new(threshold),
        },
        ScoreOrder::HigherIsBetter,
        None,
        1,
    )
    .expect("rerank node should be valid");
    let outcome = execute(&query, &mut RoutingSource::default(), 8);

    assert_eq!(outcome.points().len(), 1);
    assert_eq!(outcome.points()[0].point_id().get(), 1);
    assert_eq!(outcome.points()[0].score(), 1.8);
    assert_eq!(
        outcome
            .diagnostics()
            .iter()
            .map(StageDiagnostic::stage)
            .collect::<Vec<_>>()
            .last(),
        Some(&StageKind::Rerank)
    );
}

#[test]
fn prefetch_propagates_unavailable_sources_and_global_budget_exhaustion() {
    let query = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![branch(1.0), branch(-1.0)],
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("prefetch should be valid");
    let unavailable = execute(
        &query,
        &mut RoutingSource {
            unavailable_second_branch: true,
            ..Default::default()
        },
        8,
    );
    assert!(matches!(
        unavailable.state(),
        ExecutionState::NotReady { .. }
    ));
    assert!(unavailable.points().is_empty());

    let exhausted = execute(&query, &mut RoutingSource::default(), 3);
    assert_eq!(exhausted.completion(), Completion::BudgetExhausted);
    assert!(exhausted.points().is_empty());
    assert!(exhausted.usage().stages() <= 3);
}

#[test]
fn wrapped_filtered_branches_cannot_exceed_the_global_filter_budget() {
    let wrapped = QueryIr::new(
        QueryKind::ScoreThreshold {
            query: Box::new(filtered_branch(-1.0)),
            minimum: None,
            maximum: None,
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("wrapped filtered branch should be valid");
    let query = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![filtered_branch(1.0), wrapped],
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("prefetch should be valid");
    let mut source = RoutingSource::default();
    let mut filter = CountingFilter::default();
    let outcome = QueryExecutor::new(
        &mut source,
        Some(&mut filter),
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .execute(
        &query,
        ExecutionBudget::new(8, 2, 8, 8, 2, 3).expect("budget should be valid"),
    )
    .expect("execution should remain bounded");

    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.usage().filter_candidates(), 2);
    assert_eq!(filter.calls, 1);
}

#[test]
fn invalid_formula_fails_before_any_candidate_work() {
    let query = QueryIr::new(
        QueryKind::Formula {
            query: Box::new(branch(1.0)),
            formula: Formula::new("system($score)").expect("opaque text remains constructible"),
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("opaque formula plan should remain constructible");
    let mut source = RoutingSource::default();
    let error = QueryExecutor::new(
        &mut source,
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &NeverCancelled,
    )
    .execute(&query, budget(8))
    .expect_err("invalid executable formula should fail");

    assert!(matches!(
        error,
        QueryError::InvalidInput {
            field: "formula",
            ..
        }
    ));
    assert_eq!(source.calls, 0);
    assert_eq!(source.readiness_calls, 0);
}

#[test]
fn post_processing_applies_to_authoritative_partial_results() {
    let partial = || RoutingSource {
        partial_pages: true,
        ..Default::default()
    };
    let wrap = |kind| {
        QueryIr::new(kind, ScoreOrder::HigherIsBetter, None, 1).expect("wrapper should be valid")
    };

    let weighted = wrap(QueryKind::Weighted {
        query: Box::new(branch(1.0)),
        weight: 2.0,
    });
    let mut source = partial();
    let outcome = execute(&weighted, &mut source, 8);
    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.points()[0].score(), 1.8);

    let threshold = wrap(QueryKind::ScoreThreshold {
        query: Box::new(branch(1.0)),
        minimum: Some(0.5),
        maximum: None,
    });
    let mut source = partial();
    let outcome = execute(&threshold, &mut source, 8);
    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.points().len(), 1);

    let formula = wrap(QueryKind::Formula {
        query: Box::new(branch(1.0)),
        formula: Formula::new("$score + 1").expect("formula should be valid"),
    });
    let mut source = partial();
    let outcome = execute(&formula, &mut source, 8);
    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.points()[0].score(), 1.9);

    let rerank = wrap(QueryKind::Rerank {
        query: Box::new(branch(1.0)),
    });
    let mut source = partial();
    let outcome = execute(&rerank, &mut source, 8);
    assert_eq!(outcome.completion(), Completion::BudgetExhausted);
    assert_eq!(outcome.points().len(), 1);
}

#[test]
fn transform_cancellation_never_returns_points() {
    let query = QueryIr::new(
        QueryKind::Weighted {
            query: Box::new(branch(1.0)),
            weight: 2.0,
        },
        ScoreOrder::HigherIsBetter,
        None,
        3,
    )
    .expect("weighted query should be valid");
    let cancellation = CancelOnCall {
        calls: Cell::new(0),
        call: 8,
    };
    let outcome = QueryExecutor::new(
        &mut RoutingSource::default(),
        None,
        &mut ExactRechecker,
        &mut Diagnostics::default(),
        &cancellation,
    )
    .execute(&query, budget(8))
    .expect("cancellation should be an outcome");

    assert_eq!(outcome.completion(), Completion::Cancelled);
    assert!(outcome.points().is_empty());
}
