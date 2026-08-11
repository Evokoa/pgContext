//! Bounded synchronous query orchestration.

use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::time::Instant;

use crate::{
    BranchContribution, BudgetUsage, Cancellation, Candidate, CandidateSource, Completion,
    ExecutionBudget, ExecutionOutcome, ExecutionState, ExternalReranker, FilterCandidateSource,
    PointId, PortBudget, QueryClock, QueryError, QueryIr, Result, SourceReadiness, SourceRechecker,
    StageDiagnostic, StageKind, TelemetrySink, TopologyExpander, types::deterministic_points,
};

mod composite;

#[derive(Clone, Copy)]
struct ExecutionDeadline {
    started_micros: Option<u64>,
    started_wall: Instant,
    max_elapsed_micros: u64,
}

/// Pure executor composed from owned synchronous query ports.
pub struct QueryExecutor<'a> {
    candidates: &'a mut dyn CandidateSource,
    filter: Option<&'a mut dyn FilterCandidateSource>,
    rechecker: &'a mut dyn SourceRechecker,
    telemetry: &'a mut dyn TelemetrySink,
    cancellation: &'a dyn Cancellation,
    clock: Option<&'a dyn QueryClock>,
    external_reranker: Option<&'a mut dyn ExternalReranker>,
    topology_expander: Option<&'a mut dyn TopologyExpander>,
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
            clock: None,
            external_reranker: None,
            topology_expander: None,
        }
    }

    /// Attaches a monotonic clock for elapsed-budget enforcement.
    #[must_use]
    pub fn with_clock(mut self, clock: &'a dyn QueryClock) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Attaches the sole model-backed reranking adapter for this execution.
    #[must_use]
    pub fn with_external_reranker(mut self, reranker: &'a mut dyn ExternalReranker) -> Self {
        self.external_reranker = Some(reranker);
        self
    }

    /// Attaches the sole topology-expansion adapter for this execution.
    #[must_use]
    pub fn with_topology_expander(mut self, expander: &'a mut dyn TopologyExpander) -> Self {
        self.topology_expander = Some(expander);
        self
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
        let deadline = ExecutionDeadline {
            started_micros: self.clock.map(QueryClock::now_micros),
            started_wall: Instant::now(),
            max_elapsed_micros: budget.max_elapsed_micros(),
        };
        if query.limit() > budget.max_results() {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                Vec::new(),
                BudgetUsage::default(),
            ));
        }
        let mut outcome = if is_composite(query) {
            self.execute_composite(query, budget, deadline)?
        } else {
            self.execute_leaf(query, budget, deadline)?
        };
        let mut usage = outcome.usage();
        if self.checkpoint(deadline, &mut usage)? == Some(Completion::BudgetExhausted) {
            outcome.exhaust_budget();
        }
        outcome.set_elapsed_micros(usage.elapsed_micros());
        Ok(outcome)
    }

    fn execute_leaf(
        &mut self,
        query: &QueryIr,
        budget: ExecutionBudget,
        deadline: ExecutionDeadline,
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

        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }

        let port_budget = self.port_budget(budget, usage, deadline)?;
        let readiness = self.candidates.readiness(query, port_budget)?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
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
            let port_budget = self.port_budget(budget, usage, deadline)?;
            let filter_limit = self
                .filter
                .as_deref_mut()
                .ok_or(QueryError::PortFailure {
                    stage: "filter_candidate_source",
                    message: "query has a filter but no filter adapter is available".to_owned(),
                })?
                .candidate_limit(query, budget.max_filter_candidates(), port_budget)?;
            if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
                return Ok(outcome(completion, Vec::new(), diagnostics, usage));
            }
            if filter_limit == 0 || filter_limit > budget.max_filter_candidates() {
                return Err(QueryError::PortFailure {
                    stage: "filter_candidate_source",
                    message: format!(
                        "filter candidate request {filter_limit} is outside remaining budget {}",
                        budget.max_filter_candidates()
                    ),
                });
            }
            let port_budget = self.port_budget(budget, usage, deadline)?;
            let batch = self
                .filter
                .as_deref_mut()
                .ok_or(QueryError::PortFailure {
                    stage: "filter_candidate_source",
                    message: "filter adapter disappeared between bounded calls".to_owned(),
                })?
                .filter_candidates(query, filter_limit, port_budget)?;
            if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
                return Ok(outcome(completion, Vec::new(), diagnostics, usage));
            }
            if batch.point_ids().len() > filter_limit {
                return Err(contract_violation(
                    "filter_candidate_source",
                    filter_limit,
                    batch.point_ids().len(),
                ));
            }
            reject_duplicate_point_ids("filter_candidate_source", batch.point_ids())?;
            if batch.evaluated_count() > port_budget.max_comparisons() {
                return Err(contract_violation(
                    "filter_comparisons",
                    port_budget.max_comparisons(),
                    batch.evaluated_count(),
                ));
            }
            usage.add_filter_candidates(batch.point_ids().len());
            usage.add_memory_bytes(batch.point_ids().len().saturating_mul(size_of::<PointId>()));
            usage.add_comparisons(batch.evaluated_count());
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
            if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
                return Ok(outcome(completion, Vec::new(), diagnostics, usage));
            }
            if !batch.exhausted()
                || usage.stages() >= budget.max_stages()
                || budget.resources_depleted(usage)
            {
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

        let port_budget = self.port_budget(budget, usage, deadline)?;
        let candidate_limit =
            self.candidates
                .candidate_limit(query, budget.max_candidates(), port_budget)?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }
        if candidate_limit == 0 || candidate_limit > budget.max_candidates() {
            return Err(QueryError::PortFailure {
                stage: "candidate_source",
                message: format!(
                    "candidate request {candidate_limit} is outside remaining budget {}",
                    budget.max_candidates()
                ),
            });
        }
        let port_budget = self.port_budget(budget, usage, deadline)?;
        let page = self.candidates.candidates(
            query,
            filter_batch.as_ref(),
            candidate_limit,
            port_budget,
        )?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }
        if page.candidates().len() > candidate_limit {
            return Err(contract_violation(
                "candidate_source",
                candidate_limit,
                page.candidates().len(),
            ));
        }
        if page.candidate_work_count() < page.candidates().len() {
            return Err(contract_violation(
                "candidate_work_minimum",
                page.candidates().len(),
                page.candidate_work_count(),
            ));
        }
        if page.candidate_work_count() > candidate_limit {
            return Err(contract_violation(
                "candidate_work",
                candidate_limit,
                page.candidate_work_count(),
            ));
        }
        if page.expansion_count() > budget.max_expansions() {
            return Err(contract_violation(
                "candidate_expansions",
                budget.max_expansions(),
                page.expansion_count(),
            ));
        }
        if page.scored_count() > port_budget.max_comparisons() {
            return Err(contract_violation(
                "candidate_comparisons",
                port_budget.max_comparisons(),
                page.scored_count(),
            ));
        }
        let page_memory_bytes = page
            .candidates()
            .len()
            .checked_mul(size_of::<Candidate>())
            .and_then(|bytes| bytes.checked_add(page.retained_memory_bytes()))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "candidate_page_memory_accounting",
            })?;
        if page_memory_bytes > port_budget.max_memory_bytes() {
            return Err(contract_violation(
                "candidate_memory",
                port_budget.max_memory_bytes(),
                page_memory_bytes,
            ));
        }
        if !page.stage_diagnostics().is_empty() {
            let expected_stages = page.expansion_count().saturating_add(1);
            if page.stage_diagnostics().len() != expected_stages {
                return Err(contract_violation(
                    "candidate_stage_count",
                    expected_stages,
                    page.stage_diagnostics().len(),
                ));
            }
            let diagnostic_comparisons =
                page.stage_diagnostics()
                    .iter()
                    .try_fold(0_usize, |total, diagnostic| {
                        total.checked_add(diagnostic.input_count()).ok_or(
                            QueryError::ArithmeticOverflow {
                                operation: "candidate_stage_comparison_accounting",
                            },
                        )
                    })?;
            if diagnostic_comparisons != page.scored_count() {
                return Err(contract_violation(
                    "candidate_stage_comparisons",
                    page.scored_count(),
                    diagnostic_comparisons,
                ));
            }
            let diagnostic_candidates =
                page.stage_diagnostics()
                    .iter()
                    .try_fold(0_usize, |total, diagnostic| {
                        total.checked_add(diagnostic.output_count()).ok_or(
                            QueryError::ArithmeticOverflow {
                                operation: "candidate_stage_candidate_accounting",
                            },
                        )
                    })?;
            if diagnostic_candidates != page.candidate_work_count() {
                return Err(contract_violation(
                    "candidate_stage_candidates",
                    page.candidate_work_count(),
                    diagnostic_candidates,
                ));
            }
        }
        reject_duplicate_candidates("candidate_source", page.candidates())?;
        usage.add_candidates(page.candidate_work_count());
        usage.add_expansions(page.expansion_count());
        usage.add_comparisons(page.scored_count());
        usage.add_memory_bytes(page_memory_bytes);
        let candidate_stage_count = page.stage_diagnostics().len().max(1);
        if usage.stages().saturating_add(candidate_stage_count) > budget.max_stages() {
            return Err(contract_violation(
                "candidate_stages",
                budget.max_stages().saturating_sub(usage.stages()),
                candidate_stage_count,
            ));
        }
        for _ in 0..candidate_stage_count {
            usage.add_stage();
        }
        let mut completion = if page.exhausted() {
            Completion::Complete
        } else {
            Completion::BudgetExhausted
        };
        if page.stage_diagnostics().is_empty() {
            let diagnostic = StageDiagnostic::new(
                StageKind::Candidates,
                page.strategy(),
                page.scored_count(),
                page.candidate_work_count(),
                None,
            );
            self.telemetry.record(&diagnostic)?;
            diagnostics.push(diagnostic);
        } else {
            for candidate_diagnostic in page.stage_diagnostics() {
                let mut diagnostic = StageDiagnostic::new(
                    StageKind::Candidates,
                    candidate_diagnostic.strategy(),
                    candidate_diagnostic.input_count(),
                    candidate_diagnostic.output_count(),
                    None,
                );
                if let Some(adaptive) = candidate_diagnostic.adaptive() {
                    diagnostic = diagnostic.with_adaptive(adaptive);
                }
                self.telemetry.record(&diagnostic)?;
                diagnostics.push(diagnostic);
            }
        }

        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }
        if page.candidates().is_empty() {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }
        if usage.memory_bytes() >= budget.max_memory_bytes()
            || usage.hydration_bytes() >= budget.max_hydration_bytes()
        {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
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
        let port_budget = self.port_budget(budget, usage, deadline)?;
        let recheck_page =
            self.rechecker
                .recheck(query, page.candidates(), recheck_limit, port_budget)?;
        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }
        if recheck_page.rows().len() > recheck_limit {
            return Err(contract_violation(
                "source_rechecker",
                recheck_limit,
                recheck_page.rows().len(),
            ));
        }
        if recheck_page.comparisons() > port_budget.max_comparisons() {
            return Err(contract_violation(
                "source_recheck_comparisons",
                port_budget.max_comparisons(),
                recheck_page.comparisons(),
            ));
        }
        let recheck_comparisons = recheck_page.comparisons();
        let rows = recheck_page.into_rows();
        reject_duplicate_hydrated("source_rechecker", &rows)?;
        let hydration_bytes = rows
            .iter()
            .map(|row| row.source_key().as_str().len())
            .sum::<usize>();
        let contribution_count = page.candidates().len();
        let provenance_bytes = contribution_count
            .saturating_mul(
                size_of::<PointId>()
                    .saturating_add(size_of::<(crate::CandidateProvenance, u32)>())
                    .saturating_add(size_of::<BranchContribution>()),
            )
            .saturating_add(
                rows.len()
                    .saturating_mul(size_of::<Vec<BranchContribution>>()),
            );
        usage.add_comparisons(recheck_comparisons);
        usage.add_hydration_bytes(hydration_bytes);
        usage.add_memory_bytes(hydrated_allocation_bytes(&rows).saturating_add(provenance_bytes));
        if budget.exhausted(usage) {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
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
        let mut provenance = BTreeMap::<_, Vec<_>>::new();
        for (rank, candidate) in page.candidates().iter().enumerate() {
            let source_rank = u32::try_from(rank).unwrap_or(u32::MAX);
            provenance
                .entry(candidate.point_id())
                .or_default()
                .push((candidate.provenance(), source_rank));
        }
        let rows: Vec<crate::HydratedCandidate> = rows
            .into_iter()
            .map(|row| {
                let contributions = provenance
                    .get(&row.point_id())
                    .into_iter()
                    .flatten()
                    .map(|(provenance, source_rank)| {
                        BranchContribution::source(*provenance, row.score(), *source_rank)
                    })
                    .collect();
                row.with_contributions(contributions)
            })
            .collect();
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

        if budget.exhausted(usage) {
            return Ok(outcome(
                Completion::BudgetExhausted,
                Vec::new(),
                diagnostics,
                usage,
            ));
        }

        if let Some(completion) = self.checkpoint(deadline, &mut usage)? {
            return Ok(outcome(completion, Vec::new(), diagnostics, usage));
        }

        Ok(outcome(completion, points, diagnostics, usage))
    }

    fn checkpoint(
        &self,
        deadline: ExecutionDeadline,
        usage: &mut BudgetUsage,
    ) -> Result<Option<Completion>> {
        if cancelled(self.cancellation)? {
            return Ok(Some(Completion::Cancelled));
        }
        let elapsed = self.elapsed_micros(deadline)?;
        usage.set_elapsed_micros(elapsed);
        Ok((elapsed >= deadline.max_elapsed_micros).then_some(Completion::BudgetExhausted))
    }

    fn port_budget(
        &self,
        budget: ExecutionBudget,
        usage: BudgetUsage,
        deadline: ExecutionDeadline,
    ) -> Result<PortBudget> {
        let elapsed = self.elapsed_micros(deadline)?;
        Ok(PortBudget::new(
            budget.max_comparisons().saturating_sub(usage.comparisons()),
            budget
                .max_memory_bytes()
                .saturating_sub(usage.memory_bytes()),
            budget
                .max_hydration_bytes()
                .saturating_sub(usage.hydration_bytes()),
            deadline.max_elapsed_micros.saturating_sub(elapsed),
        ))
    }

    fn elapsed_micros(&self, deadline: ExecutionDeadline) -> Result<u64> {
        if let (Some(started), Some(clock)) = (deadline.started_micros, self.clock) {
            return clock
                .now_micros()
                .checked_sub(started)
                .ok_or(QueryError::PortFailure {
                    stage: "query_clock",
                    message: "clock moved backwards".to_owned(),
                });
        }
        Ok(u64::try_from(deadline.started_wall.elapsed().as_micros()).unwrap_or(u64::MAX))
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
            | crate::QueryKind::ExternalRerank { .. }
            | crate::QueryKind::TopologyExpand { .. }
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

fn reject_duplicate_candidates(stage: &'static str, candidates: &[Candidate]) -> Result<()> {
    let mut point_ids = BTreeSet::new();
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| !point_ids.insert(candidate.point_id()))
    {
        return Err(QueryError::PortFailure {
            stage,
            message: format!(
                "adapter returned duplicate point ID {}",
                candidate.point_id().get()
            ),
        });
    }
    Ok(())
}

fn reject_duplicate_point_ids(stage: &'static str, point_ids: &[PointId]) -> Result<()> {
    let mut unique = BTreeSet::new();
    if let Some(point_id) = point_ids
        .iter()
        .copied()
        .find(|point_id| !unique.insert(*point_id))
    {
        return Err(QueryError::PortFailure {
            stage,
            message: format!("adapter returned duplicate point ID {}", point_id.get()),
        });
    }
    Ok(())
}

fn reject_duplicate_hydrated(stage: &'static str, rows: &[crate::HydratedCandidate]) -> Result<()> {
    let mut point_ids = BTreeSet::new();
    if let Some(row) = rows.iter().find(|row| !point_ids.insert(row.point_id())) {
        return Err(QueryError::PortFailure {
            stage,
            message: format!(
                "adapter returned duplicate point ID {}",
                row.point_id().get()
            ),
        });
    }
    Ok(())
}

fn hydrated_allocation_bytes(rows: &[crate::HydratedCandidate]) -> usize {
    rows.iter().fold(0_usize, |total, row| {
        total
            .saturating_add(size_of::<crate::HydratedCandidate>())
            .saturating_add(row.source_key().as_str().len())
            .saturating_add(
                row.contributions()
                    .len()
                    .saturating_mul(size_of::<BranchContribution>()),
            )
    })
}

fn cancelled(cancellation: &dyn Cancellation) -> Result<bool> {
    cancellation.check_interrupt()?;
    Ok(cancellation.is_cancelled())
}
