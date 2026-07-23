//! Bounded synchronous query orchestration.

use std::collections::BTreeSet;

use crate::{
    BudgetUsage, Cancellation, Candidate, CandidateSource, Completion, ExecutionBudget,
    ExecutionOutcome, ExecutionState, FilterCandidateSource, QueryError, QueryIr, Result,
    SourceReadiness, SourceRechecker, StageDiagnostic, StageKind, TelemetrySink,
    types::deterministic_points,
};

mod composite;

/// Pure executor composed from owned synchronous query ports.
pub struct QueryExecutor<'a> {
    candidates: &'a mut dyn CandidateSource,
    filter: Option<&'a mut dyn FilterCandidateSource>,
    rechecker: &'a mut dyn SourceRechecker,
    telemetry: &'a mut dyn TelemetrySink,
    cancellation: &'a dyn Cancellation,
}

impl<'a> QueryExecutor<'a> {
    /// Creates a pure executor over caller-owned adapters.
    #[must_use]
    pub fn new(
        candidates: &'a mut dyn CandidateSource,
        filter: Option<&'a mut dyn FilterCandidateSource>,
        rechecker: &'a mut dyn SourceRechecker,
        telemetry: &'a mut dyn TelemetrySink,
        cancellation: &'a dyn Cancellation,
    ) -> Self {
        Self {
            candidates,
            filter,
            rechecker,
            telemetry,
            cancellation,
        }
    }

    /// Executes one validated query with hard work limits.
    ///
    /// Cancellation is checked before and after each port call. All port DTOs
    /// are owned, so no PostgreSQL buffer pin or mmap view can escape an
    /// adapter.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid queries, adapter failures, telemetry
    /// failures, or a port returning more values than requested.
    pub fn execute(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
    ) -> Result<ExecutionOutcome> {
        query.validate()?;
        if query.limit() > budget.max_results() {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                Vec::new(),
                BudgetUsage::default(),
            ));
        }
        if is_composite(query) {
            return self.execute_composite(query, budget);
        }
        self.execute_leaf(query, budget)
    }

    fn execute_leaf(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
    ) -> Result<ExecutionOutcome> {
        let mut usage = BudgetUsage::default();
        let mut diagnostics = Vec::new();

        if query.limit() > budget.max_results() {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }

        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }

        let readiness = self.candidates.readiness(query)?;
        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        match readiness {
            SourceReadiness::Ready | SourceReadiness::Exact => {}
            SourceReadiness::RebuildRequired { reason } => {
                let diagnostic = StageDiagnostic::new(
                    StageKind::Readiness,
                    "rebuild_required",
                    0,
                    0,
                    Some(reason),
                );
                self.telemetry.record(&diagnostic)?;
                diagnostics.push(diagnostic);
                if cancelled(self.cancellation)? {
                    return Ok(ExecutionOutcome::new(
                        ExecutionState::RebuildRequired { reason },
                        Completion::Cancelled,
                        Vec::new(),
                        diagnostics,
                        usage,
                    ));
                }
                return Ok(ExecutionOutcome::new(
                    ExecutionState::RebuildRequired { reason },
                    Completion::Complete,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            SourceReadiness::NotReady { reason } => {
                let diagnostic =
                    StageDiagnostic::new(StageKind::Readiness, "not_ready", 0, 0, Some(reason));
                self.telemetry.record(&diagnostic)?;
                diagnostics.push(diagnostic);
                if cancelled(self.cancellation)? {
                    return Ok(ExecutionOutcome::new(
                        ExecutionState::NotReady { reason },
                        Completion::Cancelled,
                        Vec::new(),
                        diagnostics,
                        usage,
                    ));
                }
                return Ok(ExecutionOutcome::new(
                    ExecutionState::NotReady { reason },
                    Completion::Complete,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
        }

        let filter_batch = if query.filter().is_some() {
            let Some(filter) = self.filter.as_deref_mut() else {
                return Err(QueryError::PortFailure {
                    stage: "filter_candidate_source",
                    message: "query has a filter but no filter adapter is available".to_owned(),
                });
            };
            let filter_limit = filter.candidate_limit(query, budget.max_filter_candidates())?;
            if filter_limit == 0 || filter_limit > budget.max_filter_candidates() {
                return Err(QueryError::PortFailure {
                    stage: "filter_candidate_source",
                    message: format!(
                        "filter candidate request {filter_limit} is outside remaining budget {}",
                        budget.max_filter_candidates()
                    ),
                });
            }
            let batch = filter.filter_candidates(query, filter_limit)?;
            if cancelled(self.cancellation)? {
                return Ok(outcome(
                    Completion::Cancelled,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            if batch.point_ids().len() > filter_limit {
                return Err(contract_violation(
                    "filter_candidate_source",
                    filter_limit,
                    batch.point_ids().len(),
                ));
            }
            usage.add_filter_candidates(batch.point_ids().len());
            usage.add_stage();
            let diagnostic = StageDiagnostic::new(
                StageKind::FilterCandidates,
                if batch.exhausted() {
                    "filter_candidates_exhausted"
                } else {
                    "filter_candidates_partial"
                },
                0,
                batch.point_ids().len(),
                None,
            );
            self.telemetry.record(&diagnostic)?;
            diagnostics.push(diagnostic);
            if cancelled(self.cancellation)? {
                return Ok(outcome(
                    Completion::Cancelled,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            if !batch.exhausted() || usage.stages() >= budget.max_stages() {
                return Ok(outcome(
                    Completion::BudgetExhausted,
                    Vec::new(),
                    diagnostics,
                    usage,
                ));
            }
            Some(batch)
        } else {
            None
        };

        if usage.stages() >= budget.max_stages() {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }

        let candidate_limit = self
            .candidates
            .candidate_limit(query, budget.max_candidates())?;
        if candidate_limit == 0 || candidate_limit > budget.max_candidates() {
            return Err(QueryError::PortFailure {
                stage: "candidate_source",
                message: format!(
                    "candidate request {candidate_limit} is outside remaining budget {}",
                    budget.max_candidates()
                ),
            });
        }
        let page = self
            .candidates
            .candidates(query, filter_batch.as_ref(), candidate_limit)?;
        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        if page.candidates().len() > candidate_limit {
            return Err(contract_violation(
                "candidate_source",
                candidate_limit,
                page.candidates().len(),
            ));
        }
        if page.expansion_count() > budget.max_expansions() {
            return Err(contract_violation(
                "candidate_expansions",
                budget.max_expansions(),
                page.expansion_count(),
            ));
        }
        usage.add_candidates(page.candidates().len());
        usage.add_expansions(page.expansion_count());
        usage.add_stage();
        let mut completion = if page.exhausted() {
            Completion::Complete
        } else {
            Completion::BudgetExhausted
        };
        let diagnostic = StageDiagnostic::new(
            StageKind::Candidates,
            page.strategy(),
            page.scored_count(),
            page.candidates().len(),
            None,
        );
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);

        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        if page.candidates().is_empty() {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }
        if usage.stages() >= budget.max_stages() {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }

        let recheck_limit = budget.max_rechecks().min(page.candidates().len());
        if recheck_limit < page.candidates().len() {
            completion = Completion::BudgetExhausted;
        }
        let rows = self
            .rechecker
            .recheck(query, page.candidates(), recheck_limit)?;
        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }
        if rows.len() > recheck_limit {
            return Err(contract_violation(
                "source_rechecker",
                recheck_limit,
                rows.len(),
            ));
        }
        let candidate_ids = page
            .candidates()
            .iter()
            .map(Candidate::point_id)
            .collect::<BTreeSet<_>>();
        if let Some(row) = rows
            .iter()
            .find(|row| !candidate_ids.contains(&row.point_id()))
        {
            return Err(QueryError::UnexpectedPointId {
                stage: "source_rechecker",
                point_id: row.point_id(),
            });
        }
        // Recheck work is the number of candidate identities submitted under
        // the authoritative recheck bound, not only the rows that survive
        // MVCC/RLS/deletion filtering.
        usage.add_rechecks(recheck_limit);
        usage.add_stage();
        let points = deterministic_points(rows, query.limit(), query.score_order());
        let diagnostic = StageDiagnostic::new(
            StageKind::SourceRecheck,
            "authoritative_source_recheck",
            recheck_limit,
            points.len(),
            None,
        );
        self.telemetry.record(&diagnostic)?;
        diagnostics.push(diagnostic);

        if cancelled(self.cancellation)? {
            return Ok(outcome(
                Completion::Cancelled,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }

        Ok(outcome(completion, points, diagnostics, usage))
    }
}

fn is_composite(query: &QueryIr) -> bool {
    matches!(
        query.kind(),
        crate::QueryKind::Prefetch { .. }
            | crate::QueryKind::Weighted { .. }
            | crate::QueryKind::ScoreThreshold { .. }
            | crate::QueryKind::Formula { .. }
            | crate::QueryKind::Rerank { .. }
    )
}

fn outcome(
    completion: Completion,
    points: Vec<crate::HydratedCandidate>,
    diagnostics: Vec<StageDiagnostic>,
    usage: BudgetUsage,
) -> ExecutionOutcome {
    ExecutionOutcome::new(
        ExecutionState::Ready,
        completion,
        points,
        diagnostics,
        usage,
    )
}

fn contract_violation(stage: &'static str, requested: usize, returned: usize) -> QueryError {
    QueryError::PortContractViolation {
        stage,
        requested,
        returned,
    }
}

fn cancelled(cancellation: &dyn Cancellation) -> Result<bool> {
    cancellation.check_interrupt()?;
    Ok(cancellation.is_cancelled())
}
